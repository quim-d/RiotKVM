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

/// Fichero a crear en el exFAT recién formateado: ruta con `/` (sin la barra inicial) y contenido. Una ruta
/// acabada en `/` crea un directorio vacío.
#[derive(Debug, Clone, Copy)]
pub struct NewFile<'a> {
    pub path: &'a str,
    pub data: &'a [u8],
}

/// Marca de tiempo exFAT (campo DOS de 32 bits + centésimas), siempre en UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp {
    pub dos: u32,
    pub ms10: u8,
}

impl Timestamp {
    /// Fecha UTC -> formato exFAT (año 1980-2107).
    pub fn from_ymd_hms(y: u32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Timestamp {
        let y = y.clamp(1980, 2107);
        let dos = ((y - 1980) << 25) | (mo << 21) | (d << 16) | (h << 11) | (mi << 5) | (s / 2);
        Timestamp { dos, ms10: ((s % 2) * 100) as u8 }
    }

    pub fn now() -> Timestamp {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let days = (secs / 86400) as i64;
        let rem = secs % 86400;
        // días desde 1970-01-01 -> fecha civil (algoritmo de H. Hinnant)
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        let y = (yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as u32;
        Timestamp::from_ymd_hms(y, m, d, (rem / 3600) as u32, (rem / 60 % 60) as u32, (rem % 60) as u32)
    }
}

/// Tabla de mayúsculas expandida (la comprimida usa 0xFFFF + n = n caracteres que no cambian).
fn upcase_map() -> Vec<u16> {
    let mut map = Vec::with_capacity(65536);
    let words: Vec<u16> = UPCASE.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut i = 0;
    while i < words.len() && map.len() < 65536 {
        if words[i] == 0xFFFF && i + 1 < words.len() {
            for _ in 0..words[i + 1] {
                map.push(map.len() as u16);
            }
            i += 2;
        } else {
            map.push(words[i]);
            i += 1;
        }
    }
    while map.len() < 65536 {
        map.push(map.len() as u16);
    }
    map
}

/// NameHash de la entrada Stream Extension (sobre el nombre en mayúsculas).
pub fn name_hash(name: &[u16], up: &[u16]) -> u16 {
    let mut h: u16 = 0;
    for &c in name {
        let u = up[c as usize];
        for b in u.to_le_bytes() {
            h = h.rotate_right(1).wrapping_add(b as u16);
        }
    }
    h
}

/// Checksum de un conjunto de entradas de directorio (sin los bytes 2-3 de la primera).
pub fn entry_set_checksum(set: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for (i, &b) in set.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        c = c.rotate_right(1).wrapping_add(b as u16);
    }
    c
}

/// Conjunto File + Stream Extension + File Name para una entrada de directorio contigua.
fn entry_set(name: &str, dir: bool, first_cluster: u32, len: u64, ts: Timestamp, up: &[u16]) -> Result<Vec<u8>> {
    let u: Vec<u16> = name.encode_utf16().collect();
    if u.is_empty() || u.len() > 255 || u.iter().any(|&c| c < 0x20 || "\"*/:<>?\\|".encode_utf16().any(|x| x == c)) {
        return Err(Error::msg(format!("nombre no válido en exFAT: «{name}»")));
    }
    let names = u.len().div_ceil(15);
    let mut set = vec![0u8; 32 * (2 + names)];
    let f = &mut set[0..32];
    f[0] = 0x85;
    f[1] = (1 + names) as u8;
    f[4..6].copy_from_slice(&(if dir { 0x10u16 } else { 0x20 }).to_le_bytes());
    for o in [8, 12, 16] {
        f[o..o + 4].copy_from_slice(&ts.dos.to_le_bytes());
    }
    f[20] = ts.ms10;
    f[21] = ts.ms10;
    // desfase UTC válido y 0: las horas son UTC
    f[22] = 0x80;
    f[23] = 0x80;
    f[24] = 0x80;
    let st = &mut set[32..64];
    st[0] = 0xC0;
    // AllocationPossible (y NoFatChain = 0: las cadenas se escriben en la FAT, lo más compatible)
    st[1] = if len > 0 || dir { 0x01 } else { 0x00 };
    st[3] = u.len() as u8;
    st[4..6].copy_from_slice(&name_hash(&u, up).to_le_bytes());
    st[8..16].copy_from_slice(&len.to_le_bytes());
    st[20..24].copy_from_slice(&first_cluster.to_le_bytes());
    st[24..32].copy_from_slice(&len.to_le_bytes());
    for (k, part) in u.chunks(15).enumerate() {
        let e = &mut set[64 + 32 * k..96 + 32 * k];
        e[0] = 0xC1;
        for (i, c) in part.iter().enumerate() {
            e[2 + i * 2..4 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    let c = entry_set_checksum(&set);
    set[2..4].copy_from_slice(&c.to_le_bytes());
    Ok(set)
}

/// Árbol de directorios a partir de las rutas.
#[derive(Debug, Default)]
struct Dir<'a> {
    dirs: Vec<(String, Dir<'a>)>,
    files: Vec<(String, &'a [u8])>,
}

impl<'a> Dir<'a> {
    fn insert(&mut self, parts: &[&str], data: Option<&'a [u8]>) -> Result<()> {
        let eq = |a: &str, b: &str| a.to_lowercase() == b.to_lowercase();
        match parts {
            [] => Err(Error::msg("ruta vacía")),
            // directorio vacío (o ya creado por otra ruta)
            [name] if data.is_none() => {
                if self.files.iter().any(|(n, _)| eq(n, name)) {
                    return Err(Error::msg(format!("«{name}» es un fichero y un directorio")));
                }
                if !self.dirs.iter().any(|(n, _)| eq(n, name)) {
                    self.dirs.push((name.to_string(), Dir::default()));
                }
                Ok(())
            }
            [name] => {
                if self.files.iter().any(|(n, _)| eq(n, name)) || self.dirs.iter().any(|(n, _)| eq(n, name)) {
                    return Err(Error::msg(format!("«{name}» repetido")));
                }
                self.files.push((name.to_string(), data.unwrap_or_default()));
                Ok(())
            }
            [d, rest @ ..] => {
                if self.files.iter().any(|(n, _)| eq(n, d)) {
                    return Err(Error::msg(format!("«{d}» es un fichero y un directorio")));
                }
                let i = match self.dirs.iter().position(|(n, _)| eq(n, d)) {
                    Some(i) => i,
                    None => {
                        self.dirs.push((d.to_string(), Dir::default()));
                        self.dirs.len() - 1
                    }
                };
                self.dirs[i].1.insert(rest, data)
            }
        }
    }

    /// Bytes de las entradas de este directorio (sin contar las especiales de la raíz).
    fn entries_bytes(&self) -> u64 {
        let set = |n: &str| 32 * (2 + n.encode_utf16().count().div_ceil(15)) as u64;
        self.dirs.iter().map(|(n, _)| set(n)).sum::<u64>() + self.files.iter().map(|(n, _)| set(n)).sum::<u64>()
    }
}

/// Plan de asignación: todo contiguo a partir del primer clúster libre.
struct Alloc<'w> {
    next: u32,
    fat: &'w mut [u8],
    cl: u64,
    limit: u32,
}

impl Alloc<'_> {
    fn take(&mut self, bytes: u64) -> Result<(u32, u32)> {
        let n = bytes.div_ceil(self.cl) as u32;
        if n == 0 {
            return Ok((0, 0));
        }
        if self.next as u64 + n as u64 > self.limit as u64 + 2 {
            return Err(Error::msg("los ficheros no caben en la partición"));
        }
        let first = self.next;
        chain(self.fat, first, n);
        self.next += n;
        Ok((first, n))
    }
}

/// Escribe un directorio (ya asignado en `first`/`clusters`) y, recursivamente, su contenido.
#[allow(clippy::too_many_arguments)]
fn write_dir(dev: &mut dyn BlockDev, at_cluster: &dyn Fn(u32) -> u64, dir: &Dir, prefix: &[u8], first: u32, clusters: u32, al: &mut Alloc, ts: Timestamp, up: &[u16]) -> Result<()> {
    let cl = al.cl;
    let mut buf = prefix.to_vec();
    let mut pending: Vec<(&Dir, u32, u32)> = Vec::new();
    for (name, sub) in &dir.dirs {
        let (f, n) = al.take(sub.entries_bytes().max(1))?;
        buf.extend(entry_set(name, true, f, n as u64 * cl, ts, up)?);
        pending.push((sub, f, n));
    }
    for (name, data) in &dir.files {
        let (f, n) = al.take(data.len() as u64)?;
        buf.extend(entry_set(name, false, f, data.len() as u64, ts, up)?);
        if n > 0 {
            let mut d = data.to_vec();
            d.resize((n as u64 * cl) as usize, 0);
            write_chunked(dev, at_cluster(f), &d)?;
        }
    }
    if buf.len() as u64 > clusters as u64 * cl {
        return Err(Error::msg("directorio exFAT más grande de lo calculado"));
    }
    buf.resize((clusters as u64 * cl) as usize, 0);
    write_chunked(dev, at_cluster(first), &buf)?;
    for (sub, f, n) in pending {
        write_dir(dev, at_cluster, sub, &[], f, n, al, ts, up)?;
    }
    Ok(())
}

/// Escribe un exFAT vacío en `[offset, offset + volume_sectors*512)` de `dev`.
pub fn format(dev: &mut dyn BlockDev, offset: u64, volume_sectors: u64, cluster_sectors: u32, label: &str, serial: u32) -> Result<Geometry> {
    format_with_files(dev, offset, volume_sectors, cluster_sectors, label, serial, &[], Timestamp::now())
}

/// Como [`format`] y además crea `files` (y sus directorios), cada uno en clústeres contiguos con su cadena en
/// la FAT, el bitmap de asignación al día y los checksums de las entradas. Sin ficheros, el resultado es
/// idéntico byte a byte al de `mkexfatfs`.
#[allow(clippy::too_many_arguments)]
pub fn format_with_files(dev: &mut dyn BlockDev, offset: u64, volume_sectors: u64, cluster_sectors: u32, label: &str, serial: u32, files: &[NewFile], ts: Timestamp) -> Result<Geometry> {
    let g = geometry(volume_sectors, cluster_sectors)?;
    let label_entry = dir_entry_label(label)?;
    let at = |sector: u64| offset + sector * SECTOR;
    let cl = g.cluster_sectors as u64 * SECTOR;
    let heap = at(g.heap_offset as u64);
    let cluster_at = move |c: u32| heap + (c as u64 - 2) * cl;

    let mut tree = Dir::default();
    for f in files {
        let is_dir = f.path.ends_with('/');
        let parts: Vec<&str> = f.path.trim_start_matches('/').trim_end_matches('/').split('/').collect();
        if parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") || (is_dir && !f.data.is_empty()) {
            return Err(Error::msg(format!("ruta no válida: {}", f.path)));
        }
        tree.insert(&parts, if is_dir { None } else { Some(f.data) })?;
    }
    let up = upcase_map();

    // región de arranque principal + copia, y el hueco hasta la FAT a cero
    let br = boot_region(&g, serial);
    let mut head = vec![0u8; (g.fat_offset as u64 * SECTOR) as usize];
    head[..br.len()].copy_from_slice(&br);
    head[br.len()..2 * br.len()].copy_from_slice(&br);
    dev.write_at(at(0), &head)?;

    // FAT (a cero salvo las cadenas)
    let mut fat = vec![0u8; (g.fat_length as u64 * SECTOR) as usize];
    fat[0..4].copy_from_slice(&0xFFFF_FFF8u32.to_le_bytes());
    fat[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    chain(&mut fat, 2, g.bitmap_clusters);
    chain(&mut fat, g.upcase_cluster, g.upcase_clusters);
    let root_bytes = 96 + tree.entries_bytes();
    let root_clusters = root_bytes.div_ceil(cl) as u32;
    chain(&mut fat, g.root_cluster, root_clusters);

    let mut up_buf = vec![0u8; (g.upcase_clusters as u64 * cl) as usize];
    up_buf[..UPCASE.len()].copy_from_slice(UPCASE);
    dev.write_at(cluster_at(g.upcase_cluster), &up_buf)?;

    let mut prefix = vec![0u8; 96];
    prefix[0..32].copy_from_slice(&label_entry);
    let bme = &mut prefix[32..64];
    bme[0] = 0x81;
    bme[20..24].copy_from_slice(&2u32.to_le_bytes());
    bme[24..32].copy_from_slice(&g.bitmap_bytes.to_le_bytes());
    let upe = &mut prefix[64..96];
    upe[0] = 0x82;
    upe[4..8].copy_from_slice(&checksum(UPCASE, &[]).to_le_bytes());
    upe[20..24].copy_from_slice(&g.upcase_cluster.to_le_bytes());
    upe[24..32].copy_from_slice(&(UPCASE.len() as u64).to_le_bytes());

    let mut al = Alloc { next: g.root_cluster + root_clusters, fat: &mut fat, cl, limit: g.cluster_count };
    write_dir(dev, &cluster_at, &tree, &prefix, g.root_cluster, root_clusters, &mut al, ts, &up)?;
    let used = al.next - 2;
    write_chunked(dev, at(g.fat_offset as u64), &fat)?;

    // bitmap: todos los clústeres usados son los primeros `used`
    let mut bm = vec![0u8; (g.bitmap_clusters as u64 * cl) as usize];
    for i in 0..used as usize {
        bm[i / 8] |= 1 << (i % 8);
    }
    write_chunked(dev, cluster_at(2), &bm)?;
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

/// Lector mínimo (verificación y pruebas): lista los ficheros de un exFAT siguiendo las cadenas de la FAT y
/// comprobando los checksums de cada conjunto de entradas. Devuelve `(ruta, contenido)`; los directorios salen
/// como `("ruta/", [])`.
pub fn read_files(dev: &mut dyn BlockDev, offset: u64) -> Result<Vec<(String, Vec<u8>)>> {
    let mut vbr = vec![0u8; 24 * 512];
    dev.read_at(offset, &mut vbr)?;
    let v = read_vbr(&vbr)?;
    let cl = v.cluster_sectors as u64 * SECTOR;
    let fat_len = u32::from_le_bytes(vbr[84..88].try_into().unwrap()) as u64 * SECTOR;
    let mut fat = vec![0u8; fat_len as usize];
    dev.read_at(offset + v.fat_offset as u64 * SECTOR, &mut fat)?;
    let heap = offset + v.heap_offset as u64 * SECTOR;
    let read_chain = |dev: &mut dyn BlockDev, first: u32, len: u64, contiguous: bool| -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut c = first;
        let mut buf = vec![0u8; cl as usize];
        while (out.len() as u64) < len {
            if c < 2 || c as usize * 4 + 4 > fat.len() {
                return Err(Error::msg("cadena exFAT rota"));
            }
            dev.read_at(heap + (c as u64 - 2) * cl, &mut buf)?;
            out.extend_from_slice(&buf);
            c = if contiguous { c + 1 } else { u32::from_le_bytes(fat[c as usize * 4..c as usize * 4 + 4].try_into().unwrap()) };
        }
        out.truncate(len as usize);
        Ok(out)
    };
    let mut out = Vec::new();
    // (ruta, primer clúster, tamaño, contiguo)
    let mut stack = vec![(String::new(), v.root_cluster, u64::MAX, false)];
    while let Some((path, first, len, contiguous)) = stack.pop() {
        let len = if len == u64::MAX {
            // la raíz no tiene tamaño: se mide su cadena
            let mut n = 1u64;
            let mut c = first;
            loop {
                let nx = u32::from_le_bytes(fat[c as usize * 4..c as usize * 4 + 4].try_into().unwrap());
                if nx >= 0xFFFF_FFF7 {
                    break;
                }
                c = nx;
                n += 1;
            }
            n * cl
        } else {
            len
        };
        let d = read_chain(dev, first, len, contiguous)?;
        let mut i = 0;
        while i + 32 <= d.len() && d[i] != 0 {
            if d[i] != 0x85 {
                i += 32;
                continue;
            }
            let n = d[i + 1] as usize;
            let set = &d[i..i + 32 * (n + 1)];
            if u16::from_le_bytes([set[2], set[3]]) != entry_set_checksum(set) {
                return Err(Error::msg(format!("checksum de entrada incorrecto en «{path}»")));
            }
            let dir = u16::from_le_bytes([set[4], set[5]]) & 0x10 != 0;
            let st = &set[32..64];
            let nlen = st[3] as usize;
            let contig = st[1] & 2 != 0;
            let first = u32::from_le_bytes(st[20..24].try_into().unwrap());
            let size = u64::from_le_bytes(st[24..32].try_into().unwrap());
            let name: Vec<u16> = set[64..].chunks(32).flat_map(|e| e[2..32].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]]))).take(nlen).collect();
            let full = format!("{path}{}", String::from_utf16_lossy(&name));
            if dir {
                out.push((format!("{full}/"), Vec::new()));
                stack.push((full + "/", first, size, contig));
            } else {
                let data = if size == 0 { Vec::new() } else { read_chain(dev, first, size, contig)? };
                out.push((full, data));
            }
            i += 32 * (n + 1);
        }
    }
    out.sort();
    Ok(out)
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

    #[test]
    fn empty_format_is_unchanged_by_the_file_writer() {
        // sin ficheros, format_with_files == format (lo que ya cuadraba con mkexfatfs)
        let sectors = 200_000u64;
        let mut a = MemDev::new(sectors * 512);
        let mut b = MemDev::new(sectors * 512);
        format(&mut a, 0, sectors, 64, "Ventoy", 7).unwrap();
        format_with_files(&mut b, 0, sectors, 64, "Ventoy", 7, &[], Timestamp::from_ymd_hms(2026, 1, 2, 3, 4, 5)).unwrap();
        assert!(a.0 == b.0);
    }

    #[test]
    fn files_and_dirs_round_trip() {
        let sectors = 400_000u64;
        let mut d = MemDev::new((sectors + 2048) * 512);
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        let long = "un nombre de fichero bastante largo con acentos áéí.txt";
        let files = [
            NewFile { path: "ventoy/ventoy.json", data: b"{}" },
            NewFile { path: "ventoy/theme/qkvm_1920x1080/background.png", data: &big },
            NewFile { path: "ventoy/theme/fonts/OFL.txt", data: b"ofl" },
            NewFile { path: long, data: b"x" },
            NewFile { path: "vacio.txt", data: b"" },
            NewFile { path: "Tools/", data: b"" },
        ];
        let g = format_with_files(&mut d, 2048 * 512, sectors, 64, "Ventoy", 1, &files, Timestamp::from_ymd_hms(2026, 11, 23, 12, 0, 0)).unwrap();
        let got = read_files(&mut d, 2048 * 512).unwrap();
        let mut want: Vec<(String, Vec<u8>)> = files.iter().map(|f| (f.path.to_string(), f.data.to_vec())).collect();
        for dir in ["ventoy/", "ventoy/theme/", "ventoy/theme/qkvm_1920x1080/", "ventoy/theme/fonts/"] {
            want.push((dir.to_string(), vec![]));
        }
        want.sort();
        assert_eq!(got, want);
        // bitmap coherente: bitmap + upcase + raíz + 4 dirs + 4 ficheros con datos (el de 100 kB, 4 clústeres)
        let mut bm = vec![0u8; 512];
        d.read_at(2048 * 512 + g.heap_offset as u64 * 512, &mut bm).unwrap();
        let used: u32 = bm.iter().map(|b| b.count_ones()).sum();
        assert_eq!(used, g.bitmap_clusters + g.upcase_clusters + 1 + 5 + 4 + 3, "5 directorios (Tools vacío también ocupa un clúster)");
        // rutas no válidas y repetidas
        let bad = [NewFile { path: "a/../b", data: b"" }];
        assert!(format_with_files(&mut d, 0, sectors, 64, "V", 1, &bad, Timestamp::now()).is_err());
        let dup = [NewFile { path: "A.txt", data: b"" }, NewFile { path: "a.TXT", data: b"" }];
        assert!(format_with_files(&mut d, 0, sectors, 64, "V", 1, &dup, Timestamp::now()).is_err());
    }

    #[test]
    fn name_hash_and_timestamp() {
        let up = upcase_map();
        assert_eq!(up[b'a' as usize], b'A' as u16);
        assert_eq!(up[0xE9], 0xC9, "é -> É");
        let n = |s: &str| name_hash(&s.encode_utf16().collect::<Vec<_>>(), &up);
        assert_eq!(n("ventoy"), n("VENTOY"));
        let t = Timestamp::from_ymd_hms(2026, 11, 23, 14, 30, 7);
        assert_eq!(t.dos >> 25, 46);
        assert_eq!((t.dos >> 21) & 0xF, 11);
        assert_eq!(t.dos & 0x1F, 3);
        assert_eq!(t.ms10, 100);
    }
}
