//! Discos candidatos para preparar la microSD y las reglas de seguridad (CRÍTICO: nunca el disco interno ni el
//! de arranque; solo externos/extraíbles de tamaño razonable).
//!
//! macOS: `diskutil list -plist physical` + `diskutil info -plist diskN` (sin root).
//! Linux: `lsblk -J -b` (sin root). Windows: no soportado todavía.

use crate::{Error, Result};

/// Tamaño máximo aceptado: 512 GiB (las microSD de «512 GB» caben con margen; un SSD externo grande no).
pub const MAX_BYTES: u64 = 512 << 30;
pub const MIN_BYTES: u64 = 128 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub id: String,
    pub size: u64,
    pub content: String,
    pub label: String,
    pub mount: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Lo que el usuario escribe para confirmar: `disk6` (macOS) o `sdb` (Linux).
    pub id: String,
    /// Ruta para escribir en crudo: `/dev/rdisk6`, `/dev/sdb`.
    pub raw_path: String,
    pub size: u64,
    pub block_size: u32,
    /// Texto del fabricante/modelo (`QKVM SD Ventoy Media`, `Apple SDXC Reader`...).
    pub model: String,
    pub bus: String,
    pub internal: bool,
    pub removable: bool,
    pub writable: bool,
    /// Contiene el sistema arrancado (o su almacenamiento físico).
    pub system: bool,
    pub scheme: String,
    pub partitions: Vec<Partition>,
}

impl Disk {
    /// El dongle QKVM en modo A/C (USB MSC vendor `QKVM`, product `SD Ventoy`).
    pub fn is_qkvm(&self) -> bool {
        let m = self.model.to_ascii_lowercase();
        // RiotKVM (firmware actual) o QKVM (anterior); "SD Ventoy" es el producto en ambos
        m.contains("riotkvm") || m.contains("qkvm") || m.contains("sd ventoy")
    }

    /// Lector de tarjetas (integrado o USB).
    pub fn is_card_reader(&self) -> bool {
        let m = format!("{} {}", self.model, self.bus).to_ascii_lowercase();
        ["sdxc", "sdhc", "card reader", "cardreader", "secure digital", "sd/mmc", "mmc", "reader"].iter().any(|k| m.contains(k))
    }

    pub fn summary(&self) -> String {
        let tag = if self.is_qkvm() {
            " · dongle RiotKVM"
        } else if self.is_card_reader() {
            " · lector de tarjetas"
        } else {
            ""
        };
        format!("{} · {} · {} · {}{tag}", self.id, crate::human_size(self.size), self.model, self.bus)
    }

    /// Qué contiene (para enseñarlo antes de borrar).
    pub fn contents(&self) -> Vec<String> {
        let mut v = vec![format!("esquema: {}", if self.scheme.is_empty() { "ninguno/desconocido" } else { &self.scheme })];
        for p in &self.partitions {
            v.push(format!(
                "{}: {} {}{}{}",
                p.id,
                crate::human_size(p.size),
                p.content,
                if p.label.is_empty() { String::new() } else { format!(" «{}»", p.label) },
                if p.mount.is_empty() { String::new() } else { format!(" montado en {}", p.mount) }
            ));
        }
        v
    }
}

/// Por qué NO se puede usar un disco (`Ok` = se puede).
pub fn safety(d: &Disk) -> Result<()> {
    let no = |m: &str| Err(Error::msg(format!("{}: {m}", d.id)));
    if d.system {
        return no("contiene el sistema arrancado");
    }
    if d.internal && !d.is_card_reader() {
        return no("es un disco interno");
    }
    if !d.removable && d.internal {
        return no("no es extraíble");
    }
    if !d.writable {
        return no("es de solo lectura (¿pestaña de bloqueo de la SD?)");
    }
    if d.size > MAX_BYTES {
        return no(&format!("demasiado grande ({}, máximo {})", crate::human_size(d.size), crate::human_size(MAX_BYTES)));
    }
    if d.size < MIN_BYTES {
        return no("demasiado pequeño");
    }
    if d.block_size != 512 {
        return no(&format!("sectores de {} bytes (Ventoy y el dongle exigen 512)", d.block_size));
    }
    Ok(())
}

/// Disco a preseleccionar: el dongle QKVM y, si no, un lector de tarjetas. Nunca uno que no pase [`safety`].
pub fn default_choice(disks: &[Disk]) -> Option<usize> {
    let ok = |d: &&Disk| safety(d).is_ok();
    disks.iter().position(|d| ok(&d) && d.is_qkvm()).or_else(|| disks.iter().position(|d| ok(&d) && d.is_card_reader()))
}

/// La confirmación exige escribir el identificador exacto del disco.
pub fn confirmed(d: &Disk, typed: &str) -> bool {
    typed.trim() == d.id
}

/// Lista discos externos (los que no pasan [`safety`] también salen, para explicar por qué).
pub fn list() -> Result<Vec<Disk>> {
    #[cfg(target_os = "macos")]
    return macos::list();
    #[cfg(target_os = "linux")]
    return linux::list();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(Error::msg("listar discos aún no está soportado en este sistema (Windows)"))
}

/// Desmonta todas las particiones del disco (antes de escribir).
pub fn unmount(d: &Disk) -> Result<()> {
    #[cfg(target_os = "macos")]
    return macos::unmount(&d.id);
    #[cfg(target_os = "linux")]
    {
        for p in d.partitions.iter().filter(|p| !p.mount.is_empty()) {
            crate::rawdev::udisks_unmount(&format!("/dev/{}", p.id))?;
        }
        return Ok(());
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = d;
        Err(Error::msg("no soportado en este sistema"))
    }
}

/// Vuelve a leer un disco por su id y comprueba que sigue siendo el mismo (tamaño y modelo) antes de escribir.
pub fn recheck(d: &Disk) -> Result<Disk> {
    let now = list()?.into_iter().find(|x| x.id == d.id).ok_or_else(|| Error::msg(format!("{} ya no está conectado", d.id)))?;
    if now.size != d.size || now.model != d.model {
        return Err(Error::msg(format!("{} ha cambiado (¿otro disco en el mismo sitio?): vuelve a elegirlo", d.id)));
    }
    safety(&now)?;
    Ok(now)
}

#[cfg(any(target_os = "macos", test))]
pub mod macos {
    use super::{Disk, Partition};
    use crate::{Error, Result};

    fn run(args: &[&str]) -> Result<Vec<u8>> {
        let o = std::process::Command::new("/usr/sbin/diskutil").args(args).output().map_err(|e| Error::msg(format!("diskutil: {e}")))?;
        if !o.status.success() {
            return Err(Error::msg(format!("diskutil {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).trim())));
        }
        Ok(o.stdout)
    }

    fn dict(b: &[u8]) -> Result<plist::Dictionary> {
        plist::from_bytes(b).map_err(|e| Error::msg(format!("plist de diskutil ilegible: {e}")))
    }

    fn s(d: &plist::Dictionary, k: &str) -> String {
        d.get(k).and_then(|v| v.as_string()).unwrap_or_default().to_string()
    }
    fn b(d: &plist::Dictionary, k: &str) -> Option<bool> {
        d.get(k).and_then(|v| v.as_boolean())
    }
    fn n(d: &plist::Dictionary, k: &str) -> u64 {
        d.get(k).and_then(|v| v.as_unsigned_integer()).unwrap_or(0)
    }

    /// `diskutil list -plist physical` -> (disco, particiones).
    pub fn parse_list(b: &[u8]) -> Result<Vec<(String, String, Vec<Partition>)>> {
        let d = dict(b)?;
        let all = d.get("AllDisksAndPartitions").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        Ok(all
            .iter()
            .filter_map(|x| x.as_dictionary())
            .map(|x| {
                let parts = x
                    .get("Partitions")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|p| p.as_dictionary())
                            .map(|p| Partition { id: s(p, "DeviceIdentifier"), size: n(p, "Size"), content: s(p, "Content"), label: s(p, "VolumeName"), mount: s(p, "MountPoint") })
                            .collect()
                    })
                    .unwrap_or_default();
                (s(x, "DeviceIdentifier"), s(x, "Content"), parts)
            })
            .collect())
    }

    /// `diskutil info -plist diskN` -> `Disk` (sin particiones). `system`: discos físicos del sistema arrancado.
    pub fn parse_info(bytes: &[u8], system: &[String]) -> Result<Disk> {
        let d = dict(bytes)?;
        let id = s(&d, "DeviceIdentifier");
        if id.is_empty() || !id.starts_with("disk") || id.contains('s') && id[4..].contains('s') {
            return Err(Error::msg(format!("identificador inesperado: {id}")));
        }
        let model = [s(&d, "IORegistryEntryName"), s(&d, "MediaName")].into_iter().find(|x| !x.is_empty()).unwrap_or_default();
        Ok(Disk {
            raw_path: format!("/dev/r{id}"),
            size: n(&d, "TotalSize").max(n(&d, "Size")),
            block_size: n(&d, "DeviceBlockSize") as u32,
            model,
            bus: s(&d, "BusProtocol"),
            internal: b(&d, "Internal").unwrap_or(true),
            removable: b(&d, "RemovableMediaOrExternalDevice").or(b(&d, "Removable")).unwrap_or(false),
            writable: b(&d, "WritableMedia").unwrap_or(false),
            system: system.contains(&id) || s(&d, "VirtualOrPhysical") == "Virtual" && b(&d, "Internal").unwrap_or(true),
            scheme: s(&d, "Content"),
            partitions: Vec::new(),
            id,
        })
    }

    /// Discos físicos que sostienen `/` (APFS: los almacenes físicos del contenedor).
    #[cfg(target_os = "macos")]
    fn system_disks() -> Vec<String> {
        let mut out = Vec::new();
        let Ok(root) = run(&["info", "-plist", "/"]).and_then(|b| dict(&b)) else { return vec!["disk0".into()] };
        out.push(s(&root, "ParentWholeDisk"));
        if let Some(a) = root.get("APFSPhysicalStores").and_then(|v| v.as_array()) {
            for st in a.iter().filter_map(|x| x.as_dictionary()) {
                let p = s(st, "APFSPhysicalStore");
                if let Ok(i) = run(&["info", "-plist", &p]).and_then(|b| dict(&b)) {
                    out.push(s(&i, "ParentWholeDisk"));
                }
            }
        }
        out.retain(|x| !x.is_empty());
        out
    }

    #[cfg(target_os = "macos")]
    pub fn list() -> Result<Vec<Disk>> {
        let sys = system_disks();
        let mut v = Vec::new();
        // "physical" y no "external physical": el lector SDXC integrado de los MacBook sale como interno
        for (id, scheme, parts) in parse_list(&run(&["list", "-plist", "physical"])?)? {
            let mut d = parse_info(&run(&["info", "-plist", &id])?, &sys)?;
            if d.system || d.internal && !(d.removable && d.is_card_reader()) {
                continue;
            }
            d.partitions = parts;
            if d.scheme.is_empty() {
                d.scheme = scheme;
            }
            v.push(d);
        }
        Ok(v)
    }

    #[cfg(target_os = "macos")]
    pub fn unmount(id: &str) -> Result<()> {
        run(&["unmountDisk", id]).map(|_| ())
    }
}

#[cfg(any(target_os = "linux", test))]
pub mod linux {
    use super::{Disk, Partition};
    use crate::{Error, Result};
    use serde_json::Value;

    pub const LSBLK_COLS: &str = "NAME,PATH,SIZE,TYPE,TRAN,RM,HOTPLUG,RO,VENDOR,MODEL,LOG-SEC,MOUNTPOINT,FSTYPE,LABEL,PTTYPE";

    fn st(v: &Value, k: &str) -> String {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
    }
    fn bo(v: &Value, k: &str) -> bool {
        match v.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s == "1",
            Some(Value::Number(n)) => n.as_u64() == Some(1),
            _ => false,
        }
    }
    fn nu(v: &Value, k: &str) -> u64 {
        match v.get(k) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Salida de `lsblk -J -b -o LSBLK_COLS`.
    pub fn parse_lsblk(json: &str) -> Result<Vec<Disk>> {
        let v: Value = serde_json::from_str(json).map_err(|e| Error::msg(format!("lsblk ilegible: {e}")))?;
        let devs = v.get("blockdevices").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        Ok(devs
            .iter()
            .filter(|d| st(d, "type") == "disk")
            .map(|d| {
                let name = st(d, "name");
                let tran = st(d, "tran");
                let kids: Vec<Value> = d.get("children").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                let mounts: Vec<String> = std::iter::once(d).chain(kids.iter()).map(|p| st(p, "mountpoint")).filter(|m| !m.is_empty()).collect();
                let system = mounts.iter().any(|m| matches!(m.as_str(), "/" | "/boot" | "/boot/efi" | "/usr" | "/home" | "[SWAP]"));
                let removable = bo(d, "rm") || bo(d, "hotplug");
                Disk {
                    raw_path: { let p = st(d, "path"); if p.is_empty() { format!("/dev/{name}") } else { p } },
                    size: nu(d, "size"),
                    block_size: nu(d, "log-sec") as u32,
                    model: format!("{} {}", st(d, "vendor"), st(d, "model")).trim().to_string(),
                    bus: tran.clone(),
                    internal: !(tran == "usb" || tran == "mmc" || removable),
                    removable,
                    writable: !bo(d, "ro"),
                    system,
                    scheme: st(d, "pttype"),
                    partitions: kids
                        .iter()
                        .map(|p| Partition { id: st(p, "name"), size: nu(p, "size"), content: st(p, "fstype"), label: st(p, "label"), mount: st(p, "mountpoint") })
                        .collect(),
                    id: name,
                }
            })
            .collect())
    }

    #[cfg(target_os = "linux")]
    pub fn list() -> Result<Vec<Disk>> {
        let o = std::process::Command::new("lsblk").args(["-J", "-b", "-o", LSBLK_COLS]).output().map_err(|e| Error::msg(format!("lsblk: {e}")))?;
        if !o.status.success() {
            return Err(Error::msg(format!("lsblk: {}", String::from_utf8_lossy(&o.stderr).trim())));
        }
        Ok(parse_lsblk(&String::from_utf8_lossy(&o.stdout))?.into_iter().filter(|d| !d.internal || d.removable).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>AllDisksAndPartitions</key><array><dict>
  <key>Content</key><string>FDisk_partition_scheme</string>
  <key>DeviceIdentifier</key><string>disk6</string>
  <key>Partitions</key><array>
    <dict><key>Content</key><string>Windows_NTFS</string><key>DeviceIdentifier</key><string>disk6s1</string><key>MountPoint</key><string>/Volumes/Ventoy</string><key>Size</key><integer>62527635456</integer><key>VolumeName</key><string>Ventoy</string></dict>
    <dict><key>Content</key><string>0xEF</string><key>DeviceIdentifier</key><string>disk6s2</string><key>Size</key><integer>33554432</integer></dict>
  </array>
  <key>Size</key><integer>62562238464</integer>
</dict></array>
<key>WholeDisks</key><array><string>disk6</string></array>
</dict></plist>"#;

    fn info(id: &str, name: &str, internal: bool, size: u64, extra: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict>
<key>BusProtocol</key><string>USB</string><key>DeviceBlockSize</key><integer>512</integer>
<key>DeviceIdentifier</key><string>{id}</string><key>IORegistryEntryName</key><string>{name}</string>
<key>Internal</key><{internal}/><key>RemovableMediaOrExternalDevice</key><true/><key>WritableMedia</key><true/>
<key>TotalSize</key><integer>{size}</integer><key>VirtualOrPhysical</key><string>Physical</string>
<key>Content</key><string>FDisk_partition_scheme</string>{extra}</dict></plist>"#
        )
    }

    #[test]
    fn macos_parsing_and_safety() {
        let l = macos::parse_list(LIST.as_bytes()).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, "disk6");
        assert_eq!(l[0].2[0].label, "Ventoy");
        assert_eq!(l[0].2[0].mount, "/Volumes/Ventoy");
        let mut d = macos::parse_info(info("disk6", "QKVM SD Ventoy Media", false, 62_562_238_464, "").as_bytes(), &["disk0".into()]).unwrap();
        d.partitions = l[0].2.clone();
        assert_eq!(d.raw_path, "/dev/rdisk6");
        assert!(d.is_qkvm());
        assert!(safety(&d).is_ok());
        assert!(d.contents().iter().any(|c| c.contains("montado en /Volumes/Ventoy")));
        // el disco del sistema nunca, aunque diskutil lo diera como externo
        let sys = macos::parse_info(info("disk4", "Samsung T7", false, 500_000_000_000, "").as_bytes(), &["disk4".into()]).unwrap();
        assert!(safety(&sys).unwrap_err().to_string().contains("sistema"));
        let int = macos::parse_info(info("disk0", "APPLE SSD", true, 500_000_000_000, "").as_bytes(), &[]).unwrap();
        assert!(safety(&int).is_err());
        let big = macos::parse_info(info("disk7", "WD Elements", false, 2_000_000_000_000, "").as_bytes(), &[]).unwrap();
        assert!(safety(&big).unwrap_err().to_string().contains("grande"));
        let ro = macos::parse_info(info("disk8", "SDXC Reader", false, 32_000_000_000, "").replace("<key>WritableMedia</key><true/>", "<key>WritableMedia</key><false/>").as_bytes(), &[]).unwrap();
        assert!(safety(&ro).is_err());
        assert!(macos::parse_info(info("disk6s1", "x", false, 1 << 30, "").as_bytes(), &[]).is_err(), "solo discos enteros");
    }

    #[test]
    fn default_prefers_dongle_then_reader() {
        let mk = |id: &str, model: &str, internal: bool| Disk {
            id: id.into(),
            raw_path: format!("/dev/r{id}"),
            size: 32 << 30,
            block_size: 512,
            model: model.into(),
            bus: "USB".into(),
            internal,
            removable: true,
            writable: true,
            system: false,
            scheme: String::new(),
            partitions: vec![],
        };
        let stick = mk("disk4", "SanDisk Cruzer", false);
        let reader = mk("disk5", "Apple SDXC Reader Media", false);
        let dongle = mk("disk6", "QKVM SD Ventoy Media", false);
        assert_eq!(default_choice(&[stick.clone(), reader.clone(), dongle.clone()]), Some(2));
        assert_eq!(default_choice(&[stick.clone(), reader.clone()]), Some(1));
        assert_eq!(default_choice(std::slice::from_ref(&stick)), None, "un pendrive cualquiera no se preselecciona");
        let mut bad = dongle.clone();
        bad.system = true;
        assert_eq!(default_choice(&[bad]), None);
        assert!(mk("disk7", "RiotKVM SD Ventoy Media", false).is_qkvm(), "firmware con el nombre nuevo");
        assert!(confirmed(&dongle, " disk6\n"));
        assert!(!confirmed(&dongle, "disk"));
        assert!(!confirmed(&dongle, "DISK6"));
    }

    #[test]
    fn linux_lsblk() {
        let j = r#"{"blockdevices":[
          {"name":"nvme0n1","path":"/dev/nvme0n1","size":512110190592,"type":"disk","tran":"nvme","rm":false,"hotplug":false,"ro":false,"vendor":null,"model":"Samsung SSD","log-sec":512,"mountpoint":null,"fstype":null,"label":null,"pttype":"gpt",
           "children":[{"name":"nvme0n1p2","size":511000000000,"type":"part","mountpoint":"/","fstype":"ext4","label":null}]},
          {"name":"sdb","path":"/dev/sdb","size":62562238464,"type":"disk","tran":"usb","rm":true,"hotplug":true,"ro":false,"vendor":"QKVM    ","model":"SD Ventoy","log-sec":512,"mountpoint":null,"fstype":null,"label":null,"pttype":"dos",
           "children":[{"name":"sdb1","size":62527635456,"type":"part","mountpoint":"/media/u/Ventoy","fstype":"exfat","label":"Ventoy"}]},
          {"name":"loop0","size":1000,"type":"loop"}]}"#;
        let v = linux::parse_lsblk(j).unwrap();
        assert_eq!(v.len(), 2);
        assert!(safety(&v[0]).is_err(), "NVMe con / montado");
        assert!(v[0].system && v[0].internal);
        assert_eq!(v[1].model, "QKVM SD Ventoy");
        assert!(v[1].is_qkvm());
        assert!(safety(&v[1]).is_ok());
        assert_eq!(v[1].raw_path, "/dev/sdb");
        assert_eq!(v[1].partitions[0].mount, "/media/u/Ventoy");
    }
}
