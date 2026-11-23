//! Ventoy en modo MBR escrito por nosotros, replicando `Ventoy2Disk.sh -i` (tool/VentoyWorker.sh +
//! tool/ventoy_lib.sh `format_ventoy_disk_mbr`) de la versión fijada:
//!
//! - MBR: código de `boot/boot.img` (446 bytes), UUID de disco aleatorio en 384..400, firma de disco aleatoria en
//!   440..444, partición 1 (activa, tipo 0x07) desde el sector 2048, partición 2 (tipo 0xEF) de 65536 sectores al
//!   final con el inicio alineado a 8 sectores (4 KiB), 0x55AA.
//! - Sectores 1..2047: `boot/core.img.xz` descomprimido (GRUB, arranque legacy).
//! - Partición 2: `ventoy/ventoy.disk.img.xz` (FAT16 VTOYEFI: arranque UEFI con Secure Boot).
//! - Partición 1: exFAT «Ventoy» con clúster de 32 KiB (≤ 32 GiB) o 128 KiB (> 32 GiB), como `mkexfatfs`.
//!
//! El modo MBR arranca tanto en BIOS legacy (HP Gen8/N40L) como en UEFI. Las CHS se escriben como `fdisk`
//! (geometría 255/63, tope 1023/254/63): el script usa `parted` o `fdisk` y ambos dan CHS distintas que nadie lee.

pub mod package;
pub mod theme;

pub use package::{Package, SHA256 as PACKAGE_SHA256, VERSION};

use crate::exfat;
use crate::rawdev::BlockDev;
use crate::{Cancel, Error, Progress, Reporter, Result};

pub const SECTOR: u64 = 512;
pub const PART1_START: u64 = 2048;
/// `VENTOY_SECTOR_NUM`: partición 2 de 32 MiB.
pub const PART2_SECTORS: u64 = 65536;
pub const CORE_MAX_SECTORS: u64 = 2047;
/// MBR no direcciona más de 2^32 sectores (2 TiB).
pub const MBR_MAX_SECTORS: u64 = 1 << 32;
pub const LABEL: &str = "Ventoy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub disk_sectors: u64,
    pub part1_start: u64,
    pub part1_end: u64,
    pub part2_start: u64,
    pub part2_end: u64,
    /// Sectores por clúster del exFAT (64 = 32 KiB, 256 = 128 KiB).
    pub cluster_sectors: u32,
}

impl Plan {
    pub fn part1_sectors(&self) -> u64 {
        self.part1_end - self.part1_start + 1
    }
}

/// Particiones como `format_ventoy_disk_mbr` con `reserve_mb = 0`.
pub fn plan(disk_bytes: u64) -> Result<Plan> {
    if disk_bytes % SECTOR != 0 {
        return Err(Error::msg("el tamaño del disco no es múltiplo de 512"));
    }
    let n = disk_bytes / SECTOR;
    if n > MBR_MAX_SECTORS {
        return Err(Error::msg("el disco pasa de 2 TiB: MBR no sirve"));
    }
    // mínimo razonable: 64 MiB para ISOs además de la partición de 32 MiB
    if n < PART1_START + PART2_SECTORS + 131_072 {
        return Err(Error::msg("el disco es demasiado pequeño para Ventoy"));
    }
    let mut part1_end = n - PART2_SECTORS - 1;
    let mut part2_start = part1_end + 1;
    let m = part2_start % 8;
    if m > 0 {
        part1_end -= m;
        part2_start = part1_end + 1;
    }
    let disk_size_gb = n / 2_097_152;
    Ok(Plan {
        disk_sectors: n,
        part1_start: PART1_START,
        part1_end,
        part2_start,
        part2_end: part2_start + PART2_SECTORS - 1,
        cluster_sectors: if disk_size_gb > 32 { 256 } else { 64 },
    })
}

/// CHS de una LBA como `fdisk` (255 cabezas, 63 sectores; por encima del cilindro 1023, 1023/254/63).
pub fn chs(lba: u64) -> [u8; 3] {
    let (h, s) = (255u64, 63u64);
    let c = lba / (h * s);
    if c > 1023 {
        return [0xFE, 0xFF, 0xFF];
    }
    let head = (lba / s) % h;
    let sec = lba % s + 1;
    [head as u8, (sec as u8) | (((c >> 8) as u8 & 3) << 6), c as u8]
}

fn part_entry(boot: bool, ptype: u8, start: u64, count: u64) -> [u8; 16] {
    let mut e = [0u8; 16];
    e[0] = if boot { 0x80 } else { 0 };
    e[1..4].copy_from_slice(&chs(start));
    e[4] = ptype;
    e[5..8].copy_from_slice(&chs(start + count - 1));
    e[8..12].copy_from_slice(&(start as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(count as u32).to_le_bytes());
    e
}

/// Sector 0 completo.
pub fn build_mbr(p: &Plan, boot_img: &[u8], disk_uuid: [u8; 16], signature: [u8; 4]) -> [u8; 512] {
    let mut m = [0u8; 512];
    m[..446].copy_from_slice(&boot_img[..446]);
    m[384..400].copy_from_slice(&disk_uuid);
    m[440..444].copy_from_slice(&signature);
    m[446..462].copy_from_slice(&part_entry(true, 0x07, p.part1_start, p.part1_sectors()));
    m[462..478].copy_from_slice(&part_entry(false, 0xEF, p.part2_start, PART2_SECTORS));
    m[510] = 0x55;
    m[511] = 0xAA;
    m
}

/// Parte la tabla de particiones de un MBR (para verificar y para mostrar el contenido).
pub fn parse_mbr(m: &[u8]) -> Option<Vec<(bool, u8, u64, u64)>> {
    if m.len() < 512 || m[510..512] != [0x55, 0xAA] {
        return None;
    }
    Some(
        (0..4)
            .map(|i| &m[446 + i * 16..462 + i * 16])
            .filter(|e| e[4] != 0)
            .map(|e| (e[0] == 0x80, e[4], u32::from_le_bytes(e[8..12].try_into().unwrap()) as u64, u32::from_le_bytes(e[12..16].try_into().unwrap()) as u64))
            .collect(),
    )
}

/// Aleatorios del MBR y del exFAT (UUID de disco, firma, número de serie del volumen).
pub struct Randomness {
    pub disk_uuid: [u8; 16],
    pub signature: [u8; 4],
    pub serial: u32,
}

impl Randomness {
    pub fn new() -> Result<Randomness> {
        let mut b = [0u8; 24];
        getrandom::fill(&mut b).map_err(|e| Error::msg(format!("sin aleatoriedad del sistema: {e}")))?;
        Ok(Randomness { disk_uuid: b[..16].try_into().unwrap(), signature: b[16..20].try_into().unwrap(), serial: u32::from_le_bytes(b[20..24].try_into().unwrap()) })
    }
}

const CHUNK: usize = 1 << 20;

fn write_big(dev: &mut dyn BlockDev, off: u64, data: &[u8], label: &str, base: u64, total: u64, rep: &dyn Reporter) -> Result<()> {
    for (i, c) in data.chunks(CHUNK).enumerate() {
        let mut buf;
        let c = if c.len() % 512 != 0 {
            buf = c.to_vec();
            buf.resize(c.len().div_ceil(512) * 512, 0);
            &buf[..]
        } else {
            c
        };
        dev.write_at(off + (i * CHUNK) as u64, c)?;
        rep.progress(Progress { stage: label.into(), done: base + ((i * CHUNK + c.len()) as u64), total });
    }
    Ok(())
}

/// Escribe Ventoy entero en `dev` (disco o imagen). La única cancelación posible es antes de la primera escritura.
/// Orden: borra el inicio, partición 2, exFAT (con `files` dentro: ventoy.json y el tema), core.img y, lo último,
/// el MBR (si algo falla a medias, el disco no parece un Ventoy válido).
pub fn install(dev: &mut dyn BlockDev, pkg: &Package, rnd: &Randomness, files: &[(String, Vec<u8>)], rep: &dyn Reporter, cancel: &Cancel) -> Result<Plan> {
    let p = plan(dev.size())?;
    cancel.check()?;
    let total = 32 * 1024 + pkg.disk_img.len() as u64 + pkg.core_img.len() as u64 + 512;
    let st = "escribiendo Ventoy";
    // como el script: 32 KiB a cero al principio (MBR y restos de tablas anteriores)
    dev.write_at(0, &[0u8; 32 * 1024])?;
    rep.progress(Progress { stage: st.into(), done: 32 * 1024, total });
    write_big(dev, p.part2_start * SECTOR, &pkg.disk_img, st, 32 * 1024, total, rep)?;
    rep.progress(Progress { stage: "formateando exFAT".into(), done: 0, total: 0 });
    let nf: Vec<exfat::NewFile> = files.iter().map(|(path, data)| exfat::NewFile { path, data }).collect();
    exfat::format_with_files(dev, p.part1_start * SECTOR, p.part1_sectors(), p.cluster_sectors, LABEL, rnd.serial, &nf, exfat::Timestamp::now())?;
    let mut core = pkg.core_img.clone();
    core.resize((CORE_MAX_SECTORS * SECTOR) as usize, 0);
    write_big(dev, SECTOR, &core, st, 32 * 1024 + pkg.disk_img.len() as u64, total, rep)?;
    let mbr = build_mbr(&p, &pkg.boot_img, rnd.disk_uuid, rnd.signature);
    dev.write_at(0, &mbr)?;
    dev.sync()?;
    rep.progress(Progress { stage: st.into(), done: total, total });
    Ok(p)
}

/// Relee lo escrito y lo compara (MBR, core.img, partición 2, región de arranque exFAT y los `files` del exFAT).
pub fn verify(dev: &mut dyn BlockDev, pkg: &Package, rnd: &Randomness, files: &[(String, Vec<u8>)], rep: &dyn Reporter) -> Result<()> {
    let p = plan(dev.size())?;
    let st = "verificando (leyendo lo escrito)";
    let total = (CORE_MAX_SECTORS + 1) * SECTOR + pkg.disk_img.len() as u64;
    let mut head = vec![0u8; ((CORE_MAX_SECTORS + 1) * SECTOR) as usize];
    dev.read_at(0, &mut head)?;
    if head[..512] != build_mbr(&p, &pkg.boot_img, rnd.disk_uuid, rnd.signature) {
        return Err(Error::msg("verificación: el MBR leído no coincide con el escrito"));
    }
    let n = pkg.core_img.len();
    if head[512..512 + n] != pkg.core_img[..] || head[512 + n..].iter().any(|&b| b != 0) {
        return Err(Error::msg("verificación: core.img (sectores 1-2047) no coincide"));
    }
    rep.progress(Progress { stage: st.into(), done: head.len() as u64, total });
    let mut buf = vec![0u8; CHUNK];
    for (i, want) in pkg.disk_img.chunks(CHUNK).enumerate() {
        let b = &mut buf[..want.len()];
        dev.read_at(p.part2_start * SECTOR + (i * CHUNK) as u64, b)?;
        if b != want {
            return Err(Error::msg("verificación: la partición 2 (VTOYEFI) no coincide"));
        }
        rep.progress(Progress { stage: st.into(), done: head.len() as u64 + ((i * CHUNK + want.len()) as u64), total });
    }
    let mut vbr = vec![0u8; 24 * 512];
    dev.read_at(p.part1_start * SECTOR, &mut vbr)?;
    let v = exfat::read_vbr(&vbr)?;
    if v.volume_sectors != p.part1_sectors() || v.cluster_sectors != p.cluster_sectors {
        return Err(Error::msg("verificación: el exFAT de la partición 1 no tiene la geometría esperada"));
    }
    if !files.is_empty() {
        let got = exfat::read_files(dev, p.part1_start * SECTOR)?;
        for (path, data) in files {
            if !got.iter().any(|(gp, gd)| gp == path && gd == data) {
                return Err(Error::msg(format!("verificación: /{path} no se leyó igual en el exFAT")));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rawdev::MemDev;

    #[test]
    fn plan_matches_ventoy_script() {
        // VentoyWorker.sh/log.txt con discos de 1 GiB y 40 GiB
        let p = plan(1 << 30).unwrap();
        assert_eq!((p.part1_start, p.part1_end, p.part2_start, p.part2_end, p.cluster_sectors), (2048, 2_031_615, 2_031_616, 2_097_151, 64));
        let p = plan(40 << 30).unwrap();
        assert_eq!((p.part1_end, p.part2_start, p.part2_end, p.cluster_sectors), (83_820_543, 83_820_544, 83_886_079, 256));
        // tamaño no alineado a 8 sectores: se recorta la partición 1
        let p = plan((1 << 30) + 3 * 512).unwrap();
        assert_eq!(p.part2_start % 8, 0);
        assert_eq!(p.part2_end, p.disk_sectors - 1 - 3);
        // 32 GiB justos todavía usan clúster de 32 KiB; 33 GiB ya 128 KiB
        assert_eq!(plan(32 << 30).unwrap().cluster_sectors, 64);
        assert_eq!(plan(33 << 30).unwrap().cluster_sectors, 256);
        assert!(plan(3 << 40).is_err(), "MBR > 2 TiB");
        assert!(plan(40 << 20).is_err(), "demasiado pequeño");
    }

    #[test]
    fn chs_like_fdisk() {
        // bytes de un MBR hecho por Ventoy2Disk.sh con fdisk sobre 1 GiB
        assert_eq!(chs(2048), [0x20, 0x21, 0x00]);
        assert_eq!(chs(2_031_615), [0x75, 0x37, 0x7E]);
        assert_eq!(chs(2_031_616), [0x75, 0x38, 0x7E]);
        assert_eq!(chs(2_097_151), [0x8A, 0x08, 0x82]);
        assert_eq!(chs(83_886_079), [0xFE, 0xFF, 0xFF]);
    }

    #[test]
    fn mbr_table_like_fdisk_reference() {
        let p = plan(1 << 30).unwrap();
        let boot = [0xAAu8; 512];
        let m = build_mbr(&p, &boot, [1; 16], [2, 3, 4, 5]);
        let want_table = "8020210007753 77e0008000000f81e000075387eef8a0882 00001f0000000100".replace(' ', "");
        assert_eq!(hex::encode(&m[446..478]), want_table);
        assert_eq!(&m[384..400], &[1; 16]);
        assert_eq!(&m[440..446], &[2, 3, 4, 5, 0xAA, 0xAA]);
        assert_eq!(&m[478..512], &[&[0u8; 32][..], &[0x55, 0xAA]].concat()[..]);
        let t = parse_mbr(&m).unwrap();
        assert_eq!(t, vec![(true, 0x07, 2048, 2_029_568), (false, 0xEF, 2_031_616, 65536)]);
    }

    #[test]
    fn install_and_verify_in_memory() {
        let pkg = Package::fake();
        let rnd = Randomness::new().unwrap();
        let mut d = MemDev::new(200 << 20);
        d.0.iter_mut().for_each(|b| *b = 0x5A); // basura previa
        let files = theme::sd_files(true);
        let p = install(&mut d, &pkg, &rnd, &files, &crate::Silent, &Cancel::new()).unwrap();
        verify(&mut d, &pkg, &rnd, &files, &crate::Silent).unwrap();
        assert_eq!(parse_mbr(&d.0[..512]).unwrap()[1].2, p.part2_start);
        let got = exfat::read_files(&mut d, p.part1_start * SECTOR).unwrap();
        assert!(files.iter().all(|f| got.contains(f)), "todo lo escrito (y Tools/) se lee igual");
        assert!(got.iter().any(|(p, _)| p == "Tools/"));
        // un byte cambiado en ventoy.json se detecta
        let mut bad = files.clone();
        bad[1].1[0] ^= 1;
        assert!(verify(&mut d, &pkg, &rnd, &bad, &crate::Silent).is_err());
        // un byte cambiado en la partición 2 se detecta
        d.0[(p.part2_start * 512 + 4096) as usize] ^= 0xFF;
        assert!(verify(&mut d, &pkg, &rnd, &files, &crate::Silent).is_err());
    }

    #[test]
    fn cancel_before_writing_leaves_disk_untouched() {
        let pkg = Package::fake();
        let c = Cancel::new();
        c.cancel();
        let mut d = MemDev::new(200 << 20);
        assert_eq!(install(&mut d, &pkg, &Randomness::new().unwrap(), &[], &crate::Silent, &c).unwrap_err(), Error::Cancelled);
        assert!(d.0.iter().all(|&b| b == 0));
    }
}
