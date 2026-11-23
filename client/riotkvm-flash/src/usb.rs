//! Qué hay enchufado por USB-serie: el dongle RiotKVM (VID 0x1209, un PID por modo), la ROM de descarga de
//! Espressif (VID 0x303A: USB-Serial-JTAG 0x1001 o CDC de la ROM por USB-OTG 0x0002) u otra cosa (puentes
//! USB-UART de placas de desarrollo, etc.).

pub const RIOTKVM_VID: u16 = 0x1209;
pub const PID_MODE_C: u16 = 0x0001;
pub const PID_MODE_A: u16 = 0x0010;
pub const PID_MODE_B: u16 = 0x0011;
pub const ESPRESSIF_VID: u16 = 0x303A;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Firmware RiotKVM en marcha; modo 'A' (solo SD), 'B' (solo HID) o 'C' (todo).
    RiotKvm(char),
    /// Dispositivo Espressif por USB nativo: la ROM en modo descarga o un firmware ajeno con USB-Serial-JTAG.
    Espressif,
    /// Cualquier otro puerto serie (puede ser una placa ESP32 con puente USB-UART).
    Other,
}

pub fn classify(vid: Option<u16>, pid: Option<u16>) -> Role {
    match (vid, pid) {
        (Some(RIOTKVM_VID), Some(PID_MODE_C)) => Role::RiotKvm('C'),
        (Some(RIOTKVM_VID), Some(PID_MODE_A)) => Role::RiotKvm('A'),
        (Some(RIOTKVM_VID), Some(PID_MODE_B)) => Role::RiotKvm('B'),
        (Some(ESPRESSIF_VID), _) => Role::Espressif,
        _ => Role::Other,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub product: String,
    pub role: Role,
}

impl Port {
    pub fn label(&self) -> String {
        let id = match (self.vid, self.pid) {
            (Some(v), Some(p)) => format!("{v:04X}:{p:04X}"),
            _ => "----:----".into(),
        };
        let what = match self.role {
            Role::RiotKvm(m) => format!("dongle RiotKVM (modo {m})"),
            Role::Espressif => "Espressif USB (ROM de descarga o USB-JTAG)".into(),
            Role::Other => self.product.clone(),
        };
        format!("{} [{id}] {what}", self.name)
    }
}

/// Puertos serie USB. En macOS cada uno sale como `/dev/cu.*` y `/dev/tty.*`: se deja solo el `cu.` (no espera DCD).
pub fn scan() -> Vec<Port> {
    let mut v: Vec<Port> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(Port {
                role: classify(Some(u.vid), Some(u.pid)),
                vid: Some(u.vid),
                pid: Some(u.pid),
                product: u.product.or(u.manufacturer).unwrap_or_else(|| "USB".into()),
                name: p.port_name,
            }),
            _ => None,
        })
        .collect();
    dedup_macos(&mut v);
    v
}

fn dedup_macos(v: &mut Vec<Port>) {
    let cu: Vec<String> = v.iter().filter_map(|p| p.name.strip_prefix("/dev/cu.").map(String::from)).collect();
    v.retain(|p| p.name.strip_prefix("/dev/tty.").map(|r| !cu.iter().any(|c| c == r)).unwrap_or(true));
}

/// Puerto elegido automáticamente para flashear: un dongle RiotKVM o, si no, un Espressif USB. Varios = ambiguo.
pub fn pick(ports: &[Port]) -> Pick {
    let q: Vec<&Port> = ports.iter().filter(|p| matches!(p.role, Role::RiotKvm(_))).collect();
    let e: Vec<&Port> = ports.iter().filter(|p| p.role == Role::Espressif).collect();
    match (q.len(), e.len()) {
        (1, 0) => Pick::RiotKvm(q[0].clone()),
        (0, 1) => Pick::Rom(e[0].clone()),
        (0, 0) => Pick::Nothing,
        _ => Pick::Ambiguous(q.iter().chain(e.iter()).map(|p| p.name.clone()).collect()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    RiotKvm(Port),
    Rom(Port),
    Nothing,
    Ambiguous(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, vid: u16, pid: u16) -> Port {
        Port { name: name.into(), vid: Some(vid), pid: Some(pid), product: "x".into(), role: classify(Some(vid), Some(pid)) }
    }

    #[test]
    fn roles() {
        assert_eq!(classify(Some(0x1209), Some(0x0001)), Role::RiotKvm('C'));
        assert_eq!(classify(Some(0x1209), Some(0x0010)), Role::RiotKvm('A'));
        assert_eq!(classify(Some(0x1209), Some(0x0011)), Role::RiotKvm('B'));
        assert_eq!(classify(Some(0x1209), Some(0x0002)), Role::Other);
        assert_eq!(classify(Some(0x303A), Some(0x1001)), Role::Espressif);
        assert_eq!(classify(Some(0x303A), Some(0x0002)), Role::Espressif);
        assert_eq!(classify(Some(0x10C4), Some(0xEA60)), Role::Other);
        assert_eq!(classify(None, None), Role::Other);
    }

    #[test]
    fn picking() {
        let q = p("/dev/cu.usbmodem1", 0x1209, 0x0010);
        let r = p("/dev/cu.usbmodem2", 0x303A, 0x1001);
        let o = p("/dev/cu.usbserial", 0x10C4, 0xEA60);
        assert_eq!(pick(&[q.clone(), o.clone()]), Pick::RiotKvm(q.clone()));
        assert_eq!(pick(&[r.clone(), o.clone()]), Pick::Rom(r.clone()));
        assert_eq!(pick(std::slice::from_ref(&o)), Pick::Nothing);
        assert!(matches!(pick(&[q, r]), Pick::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn macos_tty_dedup() {
        let mut v = vec![p("/dev/cu.usbmodem1", 0x1209, 1), p("/dev/tty.usbmodem1", 0x1209, 1), p("/dev/tty.otro", 1, 1), p("COM5", 1, 1)];
        dedup_macos(&mut v);
        let n: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(n, ["/dev/cu.usbmodem1", "/dev/tty.otro", "COM5"]);
    }
}
