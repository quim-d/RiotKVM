//! riotkvm-core: núcleo del cliente del KVM de bolsillo (compartido por CLI y GUI).
//!
//! Capas: `protocol` (codifica órdenes) · `keys`/`layouts` (teclas HID, layouts del destino) ·
//! `input` (`Kvm`: teclado/ratón de alto nivel) · `script` (macros .riotkvm) · `transport`
//! (serie CDC, mock, BLE reservado).

pub mod ble_v1;
pub mod config;
pub mod error;
pub mod fw_version;
pub mod input;
pub mod keys;
pub mod latency;
pub mod layouts;
pub mod led;
pub mod name;
pub mod protocol;
pub mod rt;
pub mod scancode;
pub mod script;
pub mod session;
pub mod transport;

pub use error::{Error, Result};
pub use fw_version::FwVersion;
pub use led::Led;
pub use input::{abs_from_pixels, parse_screen, Kvm, MouseCoalescer};
pub use transport::{MockHandle, MockTransport, SerialTransport, Transport};
