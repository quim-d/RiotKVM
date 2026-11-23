//! Eventos que un transporte publica hacia la sesión/GUI.

/// Estado del enlace con el dongle (BLE: como un ratón Bluetooth, siempre intenta estar conectado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// Escaneando en busca del servicio del dongle.
    Searching,
    /// Dongle visto; conectando y descubriendo servicios.
    Connecting,
    Connected { name: String, mtu: usize },
    /// Enlace perdido o intento fallido; se reintenta con espera corta.
    Reconnecting { attempt: u32, reason: String },
}

impl LinkState {
    pub fn is_connected(&self) -> bool {
        matches!(self, LinkState::Connected { .. })
    }
    pub fn describe(&self) -> String {
        match self {
            LinkState::Searching => "buscando el dongle...".into(),
            LinkState::Connecting => "conectando...".into(),
            LinkState::Connected { name, mtu } => format!("{name} (MTU {mtu})"),
            LinkState::Reconnecting { attempt, reason } => format!("reconectando (intento {attempt}): {reason}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TEvent {
    Link(LinkState),
    /// Línea de estado del dongle empujada por EVT, en el mismo formato de `status`
    /// (`host=1 leds=0x02 hidready=1 susp=0 sd=1 ...`).
    StatusLine(String),
    /// RTT de un ping BLE, en microsegundos.
    Rtt(u64),
}
