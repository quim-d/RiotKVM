//! Formateador exFAT mínimo que reproduce lo que hace `mkexfatfs` (exfat-utils 1.3.0, el que trae Ventoy) con
//! `-n <etiqueta> -s <sectores por clúster>`: misma geometría (FAT en el sector 128, bitmap/upcase/raíz en los
//! clústeres 2.., `PartitionOffset` 0, sin código de arranque), comprobado byte a byte contra su salida.
//!
//! Hacerlo nosotros permite escribir toda la SD por un único descriptor (el de authopen/udisks2), sin depender de
//! `diskutil`/`newfs_exfat`/`mkfs.exfat`, y probarlo sobre ficheros imagen.

use crate::rawdev::BlockDev;
use crate::{Error, Result};

pub const SECTOR: u64 = 512;
/// Tabla de mayúsculas recomendada por la especificación (forma comprimida, 5836 bytes, checksum 0xE619D30D).
pub const UPCASE: &[u8] = include_bytes!("exfat_upcase.bin");
pub const UPCASE_CHECKSUM: u32 = 0xE619_D30D;
const FAT_OFFSET: u64 = 128;

/// Geometría calculada como `mkexfatfs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub volume_sectors: u64,
    pub cluster_sectors: u32,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub heap_offset: u32,
    pub cluster_count: u32,
    pub bitmap_bytes: u64,
    pub bitmap_clusters: u32,
    pub upcase_cluster: u32,
    pub upcase_clusters: u32,
    pub root_cluster: u32,
}

fn round_up(x: u64, a: u64) -> u64 {
    x.div_ceil(a) * a
}

pub fn geometry(volume_sectors: u64, cluster_sectors: u32) -> Result<Geometry> {
    if !cluster_sectors.is_power_of_two() || cluster_sectors > 65536 {
        return Err(Error::msg("sectores por clúster no válido"));
    }
    let cl = cluster_sectors as u64 * SECTOR;
    let vol = volume_sectors * SECTOR;
    // FAT alineada a 128 sectores; el bitmap (inicio del heap) alineado a clúster
    let fat_bytes = vol / cl * 4;
    let heap = round_up(FAT_OFFSET * SECTOR + fat_bytes, cl) / SECTOR;
    if heap + 4 * cluster_sectors as u64 > volume_sectors {
        return Err(Error::msg("partición demasiado pequeña para exFAT"));
    }
    let count = (volume_sectors - heap) / cluster_sectors as u64;
    if count > 0xFFFF_FFF5 || volume_sectors > u32::MAX as u64 * 64 {
        return Err(Error::msg("partición demasiado grande"));
    }
    let bitmap_bytes = count.div_ceil(8);
    let bitmap_clusters = bitmap_bytes.div_ceil(cl) as u32;
    let upcase_clusters = (UPCASE.len() as u64).div_ceil(cl) as u32;
    let upcase_cluster = 2 + bitmap_clusters;
    Ok(Geometry {
        volume_sectors,
        cluster_sectors,
        fat_offset: FAT_OFFSET as u32,
        fat_length: (heap - FAT_OFFSET) as u32,
        heap_offset: heap as u32,
        cluster_count: count as u32,
        bitmap_bytes,
        bitmap_clusters,
        upcase_cluster,
        upcase_clusters,
        root_cluster: upcase_cluster + upcase_clusters,
    })
}

/// Checksum de 32 bits de exFAT (rotación a la derecha + suma). `skip` excluye índices (campos volátiles del VBR).
pub fn checksum(data: &[u8], skip: &[usize]) -> u32 {
    let mut c: u32 = 0;
    for (i, &b) in data.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        c = c.rotate_right(1).wrapping_add(b as u32);
    }
    c
}

/// Región de arranque (12 sectores: VBR, 8 extendidos, OEM, reservado, checksum).
pub fn boot_region(g: &Geometry, serial: u32) -> Vec<u8> {
    let mut r = vec![0u8; 12 * SECTOR as usize];
    let b = &mut r[..512];
    b[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
    b[3..11].copy_from_slice(b"EXFAT   ");
    // 11..64 a cero (MustBeZero); 64: PartitionOffset = 0 como mkexfatfs
    b[72..80].copy_from_slice(&g.volume_sectors.to_le_bytes());
    b[80..84].copy_from_slice(&g.fat_offset.to_le_bytes());
    b[84..88].copy_from_slice(&g.fat_length.to_le_bytes());
    b[88..92].copy_from_slice(&g.heap_offset.to_le_bytes());
    b[92..96].copy_from_slice(&g.cluster_count.to_le_bytes());
    b[96..100].copy_from_slice(&g.root_cluster.to_le_bytes());
    b[100..104].copy_from_slice(&serial.to_le_bytes());
    b[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
    // 106..108 VolumeFlags = 0
    b[108] = 9; // 512 bytes/sector
    b[109] = g.cluster_sectors.trailing_zeros() as u8;
    b[110] = 1; // una FAT
    b[111] = 0x80;
    // 112 PercentInUse = 0; 120..510 sin código de arranque
    b[510] = 0x55;
    b[511] = 0xAA;
    for s in 1..=8 {
        let e = (s + 1) * 512;
        r[e - 2] = 0x55;
        r[e - 1] = 0xAA;
    }
    let c = checksum(&r[..11 * 512], &[106, 107, 112]);
    for i in 0..128 {
        r[11 * 512 + i * 4..11 * 512 + i * 4 + 4].copy_from_slice(&c.to_le_bytes());
    }
    r
}

fn dir_entry_label(label: &str) -> Result<[u8; 32]> {
    let u: Vec<u16> = label.encode_utf16().collect();
    if u.is_empty() || u.len() > 11 {
        return Err(Error::msg("etiqueta exFAT de 1 a 11 caracteres"));
    }
    let mut e = [0u8; 32];
    e[0] = 0x83;
    e[1] = u.len() as u8;
    for (i, c) in u.iter().enumerate() {
        e[2 + i * 2..4 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    Ok(e)
}

/// Cadena FAT contigua `first..first+n` terminada en EOC.
fn chain(fat: &mut [u8], first: u32, n: u32) {
    for k in 0..n {
        let c = first + k;
        let v = if k + 1 == n { 0xFFFF_FFFFu32 } else { c + 1 };
        fat[c as usize * 4..c as usize * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Escribe un exFAT vacío en `[offset, offset + volume_sectors*512)` de `dev`.
pub fn format(dev: &mut dyn BlockDev, offset: u64, volume_sectors: u64, cluster_sectors: u32, label: &str, serial: u32) -> Result<Geometry> {
    let g = geometry(volume_sectors, cluster_sectors)?;
    let label_entry = dir_entry_label(label)?;
    let at = |sector: u64| offset + sector * SECTOR;
    let cl = g.cluster_sectors as u64 * SECTOR;
    let cluster_at = |c: u32| at(g.heap_offset as u64) + (c as u64 - 2) * cl;

    // región de arranque principal + copia, y el hueco hasta la FAT a cero
    let br = boot_region(&g, serial);
    let mut head = vec![0u8; (g.fat_offset as u64 * SECTOR) as usize];
    head[..br.len()].copy_from_slice(&br);
    head[br.len()..2 * br.len()].copy_from_slice(&br);
    dev.write_at(at(0), &head)?;

    // FAT entera (a cero salvo las cadenas)
    let mut fat = vec![0u8; (g.fat_length as u64 * SECTOR) as usize];
    fat[0..4].copy_from_slice(&0xFFFF_FFF8u32.to_le_bytes());
    fat[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    chain(&mut fat, 2, g.bitmap_clusters);
    chain(&mut fat, g.upcase_cluster, g.upcase_clusters);
    chain(&mut fat, g.root_cluster, 1);
    write_chunked(dev, at(g.fat_offset as u64), &fat)?;

    // bitmap: marcados los clústeres usados (bitmap + upcase + raíz)
    let used = g.bitmap_clusters + g.upcase_clusters + 1;
    let mut bm = vec![0u8; (g.bitmap_clusters as u64 * cl) as usize];
    for i in 0..used as usize {
        bm[i / 8] |= 1 << (i % 8);
    }
    write_chunked(dev, cluster_at(2), &bm)?;

    let mut up = vec![0u8; (g.upcase_clusters as u64 * cl) as usize];
    up[..UPCASE.len()].copy_from_slice(UPCASE);
    dev.write_at(cluster_at(g.upcase_cluster), &up)?;

    let mut root = vec![0u8; cl as usize];
    root[0..32].copy_from_slice(&label_entry);
    let bme = &mut root[32..64];
    bme[0] = 0x81;
    bme[20..24].copy_from_slice(&2u32.to_le_bytes());
    bme[24..32].copy_from_slice(&g.bitmap_bytes.to_le_bytes());
    let upe = &mut root[64..96];
    upe[0] = 0x82;
    upe[4..8].copy_from_slice(&checksum(UPCASE, &[]).to_le_bytes());
    upe[20..24].copy_from_slice(&g.upcase_cluster.to_le_bytes());
    upe[24..32].copy_from_slice(&(UPCASE.len() as u64).to_le_bytes());
    dev.write_at(cluster_at(g.root_cluster), &root)?;
    Ok(g)
}

fn write_chunked(dev: &mut dyn BlockDev, off: u64, data: &[u8]) -> Result<()> {
    for (i, c) in data.chunks(1 << 20).enumerate() {
        dev.write_at(off + (i as u64) * (1 << 20), c)?;
    }
    Ok(())
}

/// Lo que se lee del VBR de un exFAT (verificación tras escribir).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VbrInfo {
    pub volume_sectors: u64,
    pub cluster_sectors: u32,
    pub root_cluster: u32,
    pub heap_offset: u32,
    pub fat_offset: u32,
}

/// Comprueba firma, checksum y copia de seguridad de la región de arranque.
pub fn read_vbr(region24: &[u8]) -> Result<VbrInfo> {
    if region24.len() < 24 * 512 {
        return Err(Error::msg("región de arranque exFAT incompleta"));
    }
    let b = &region24[..512];
    if &b[3..11] != b"EXFAT   " || b[510..512] != [0x55, 0xAA] {
        return Err(Error::msg("la partición 1 no tiene un exFAT válido"));
    }
    let c = checksum(&region24[..11 * 512], &[106, 107, 112]);
    if region24[11 * 512..12 * 512].chunks(4).any(|x| x != c.to_le_bytes()) {
        return Err(Error::msg("checksum de la región de arranque exFAT incorrecto"));
    }
    if region24[..12 * 512] != region24[12 * 512..24 * 512] {
        return Err(Error::msg("la copia de la región de arranque exFAT no coincide"));
    }
    let u32at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    Ok(VbrInfo {
        volume_sectors: u64::from_le_bytes(b[72..80].try_into().unwrap()),
        fat_offset: u32at(80),
        heap_offset: u32at(88),
        root_cluster: u32at(96),
        cluster_sectors: 1 << b[109],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rawdev::MemDev;

    #[test]
    fn upcase_table_is_the_standard_one() {
        assert_eq!(UPCASE.len(), 5836);
        assert_eq!(checksum(UPCASE, &[]), UPCASE_CHECKSUM);
    }

    #[test]
    fn geometry_matches_mkexfatfs() {
        // valores leídos de mkexfatfs 1.3.0 (Ventoy2Disk.sh) sobre discos de 1 GiB y 40 GiB
        let g = geometry(2_029_568, 64).unwrap();
        assert_eq!((g.fat_offset, g.fat_length, g.heap_offset, g.cluster_count, g.root_cluster), (128, 256, 384, 31706, 4));
        assert_eq!(g.bitmap_bytes, 3964);
        let g = geometry(83_818_496, 256).unwrap();
        assert_eq!((g.fat_offset, g.fat_length, g.heap_offset, g.cluster_count, g.root_cluster), (128, 2688, 2816, 327_405, 4));
        assert_eq!(g.bitmap_bytes, 40926);
        // 32 GB con clúster de 32 KiB: el bitmap ocupa varios clústeres
        let g = geometry(62_000_000, 64).unwrap();
        assert!(g.bitmap_clusters > 1);
        assert_eq!(g.root_cluster, 2 + g.bitmap_clusters + 1);
    }

    #[test]
    fn format_and_read_back() {
        let sectors = 200_000u64;
        let mut d = MemDev::new((sectors + 2048) * 512);
        let g = format(&mut d, 2048 * 512, sectors, 64, "Ventoy", 0x1234_5678).unwrap();
        let mut reg = vec![0u8; 24 * 512];
        d.read_at(2048 * 512, &mut reg).unwrap();
        let v = read_vbr(&reg).unwrap();
        assert_eq!(v.volume_sectors, sectors);
        assert_eq!(v.cluster_sectors, 64);
        assert_eq!(v.root_cluster, g.root_cluster);
        // entrada de etiqueta en la raíz
        let mut root = vec![0u8; 512];
        d.read_at(2048 * 512 + (g.heap_offset as u64 + 2 * 64) * 512, &mut root).unwrap();
        assert_eq!(root[0], 0x83);
        assert_eq!(&root[2..14], &"Ventoy".encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>()[..]);
        reg[100] ^= 1;
        assert!(read_vbr(&reg).is_err(), "checksum");
        assert!(dir_entry_label("demasiado-largo").is_err());
    }
}
