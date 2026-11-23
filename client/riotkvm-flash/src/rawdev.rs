//! Acceso por bloques a un disco o a un fichero imagen.
//!
//! - Fichero imagen: se abre directamente (pruebas; nunca pide permisos).
//! - macOS: `/usr/libexec/authopen -stdoutpipe -o O_RDWR /dev/rdiskN` pide la contraseña de administrador UNA vez
//!   (diálogo del sistema) y nos pasa el descriptor ya abierto por un socket (SCM_RIGHTS). La app no corre como root.
//! - Linux: udisks2 `Block.OpenDevice("rw")` por D-Bus (polkit pide la autorización) y devuelve el descriptor.
//! - Windows: aún no soportado.
//!
//! Todas las E/S van alineadas a 512 bytes (los dispositivos crudos de macOS lo exigen).

use crate::{Error, Result};
use std::fs::File;
use std::path::Path;

pub trait BlockDev {
    fn size(&self) -> u64;
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()>;
    fn write_at(&mut self, off: u64, data: &[u8]) -> Result<()>;
    fn sync(&mut self) -> Result<()>;
}

fn check_align(off: u64, len: usize) -> Result<()> {
    if off % 512 != 0 || len % 512 != 0 {
        return Err(Error::msg(format!("E/S no alineada a 512 bytes (offset {off}, {len} bytes)")));
    }
    Ok(())
}

/// Fichero normal o descriptor de un dispositivo.
pub struct FileDev {
    f: File,
    size: u64,
    pub desc: String,
}

impl FileDev {
    pub fn from_file(f: File, size: u64, desc: impl Into<String>) -> FileDev {
        FileDev { f, size, desc: desc.into() }
    }

    /// Fichero imagen existente (su tamaño es el del «disco»).
    pub fn open_image(path: &Path) -> Result<FileDev> {
        let f = std::fs::OpenOptions::new().read(true).write(true).open(path).map_err(|e| Error::msg(format!("{}: {e}", path.display())))?;
        let size = f.metadata()?.len();
        if !f.metadata()?.is_file() {
            return Err(Error::msg(format!("{} no es un fichero imagen", path.display())));
        }
        Ok(FileDev { f, size, desc: path.display().to_string() })
    }
}

#[cfg(unix)]
impl BlockDev for FileDev {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        check_align(off, buf.len())?;
        self.f.read_exact_at(buf, off).map_err(|e| Error::msg(format!("lectura en {off}: {e}")))
    }
    fn write_at(&mut self, off: u64, data: &[u8]) -> Result<()> {
        use std::os::unix::fs::FileExt;
        check_align(off, data.len())?;
        if off + data.len() as u64 > self.size {
            return Err(Error::msg("escritura fuera del disco"));
        }
        self.f.write_all_at(data, off).map_err(|e| Error::msg(format!("escritura en {off}: {e}")))
    }
    fn sync(&mut self) -> Result<()> {
        self.f.sync_all().map_err(|e| Error::msg(format!("sync: {e}")))
    }
}

#[cfg(windows)]
impl BlockDev for FileDev {
    fn size(&self) -> u64 {
        self.size
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        use std::os::windows::fs::FileExt;
        check_align(off, buf.len())?;
        let mut done = 0;
        while done < buf.len() {
            let n = self.f.seek_read(&mut buf[done..], off + done as u64)?;
            if n == 0 {
                return Err(Error::msg("lectura corta"));
            }
            done += n;
        }
        Ok(())
    }
    fn write_at(&mut self, off: u64, data: &[u8]) -> Result<()> {
        use std::os::windows::fs::FileExt;
        check_align(off, data.len())?;
        let mut done = 0;
        while done < data.len() {
            done += self.f.seek_write(&data[done..], off + done as u64)?;
        }
        Ok(())
    }
    fn sync(&mut self) -> Result<()> {
        Ok(self.f.sync_all()?)
    }
}

/// Disco en memoria (pruebas).
pub struct MemDev(pub Vec<u8>);

impl MemDev {
    pub fn new(size: u64) -> MemDev {
        MemDev(vec![0; size as usize])
    }
}

impl BlockDev for MemDev {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> Result<()> {
        check_align(off, buf.len())?;
        let o = off as usize;
        buf.copy_from_slice(self.0.get(o..o + buf.len()).ok_or_else(|| Error::msg("lectura fuera"))?);
        Ok(())
    }
    fn write_at(&mut self, off: u64, data: &[u8]) -> Result<()> {
        check_align(off, data.len())?;
        let o = off as usize;
        self.0.get_mut(o..o + data.len()).ok_or_else(|| Error::msg("escritura fuera"))?.copy_from_slice(data);
        Ok(())
    }
    fn sync(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Abre el disco entero para lectura/escritura pidiendo permisos al sistema. `raw_path`: `/dev/rdiskN`, `/dev/sdX`.
#[cfg(target_os = "macos")]
pub fn open_device(raw_path: &str, size: u64) -> Result<FileDev> {
    Ok(FileDev::from_file(macos::authopen(raw_path)?, size, raw_path))
}

#[cfg(target_os = "linux")]
pub fn open_device(raw_path: &str, size: u64) -> Result<FileDev> {
    Ok(FileDev::from_file(linux::udisks_open(raw_path)?, size, raw_path))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn open_device(_raw_path: &str, _size: u64) -> Result<FileDev> {
    Err(Error::msg("escribir la SD aún no está soportado en este sistema (Windows): usa macOS o Linux"))
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::{Error, Result};
    use std::fs::File;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Command, Stdio};

    /// authopen abre `path` con permisos de administrador y nos manda el descriptor por el socket (stdout).
    pub fn authopen(path: &str) -> Result<File> {
        if !path.starts_with("/dev/rdisk") {
            return Err(Error::msg(format!("authopen solo para /dev/rdiskN, no {path}")));
        }
        let (ours, theirs) = UnixStream::pair()?;
        let child = Command::new("/usr/libexec/authopen")
            .args(["-stdoutpipe", "-o", &libc::O_RDWR.to_string(), path])
            .stdin(Stdio::null())
            .stdout(Stdio::from(OwnedFd::from(theirs)))
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::msg(format!("no se pudo lanzar authopen: {e}")))?;
        let fd = recv_fd(&ours);
        let out = child.wait_with_output()?;
        match fd {
            Some(fd) if out.status.success() => Ok(unsafe { File::from_raw_fd(fd) }),
            Some(fd) => {
                unsafe { libc::close(fd) };
                Err(Error::msg(format!("authopen falló: {}", String::from_utf8_lossy(&out.stderr).trim())))
            }
            None => Err(Error::msg(format!(
                "no se obtuvo permiso para escribir {path} (¿cancelaste la contraseña?) {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }

    fn recv_fd(sock: &UnixStream) -> Option<i32> {
        let mut data = [0u8; 64];
        let mut iov = libc::iovec { iov_base: data.as_mut_ptr() as *mut _, iov_len: data.len() };
        let mut cbuf = [0u8; 64];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cbuf.as_mut_ptr() as *mut _;
        msg.msg_controllen = cbuf.len() as _;
        let n = unsafe { libc::recvmsg(sock.as_raw_fd(), &mut msg, 0) };
        if n < 0 {
            return None;
        }
        unsafe {
            let mut c = libc::CMSG_FIRSTHDR(&msg);
            while !c.is_null() {
                if (*c).cmsg_level == libc::SOL_SOCKET && (*c).cmsg_type == libc::SCM_RIGHTS {
                    return Some(std::ptr::read_unaligned(libc::CMSG_DATA(c) as *const i32));
                }
                c = libc::CMSG_NXTHDR(&msg, c);
            }
        }
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use crate::{Error, Result};
    use dbus::arg::PropMap;
    use dbus::blocking::Connection;
    use std::fs::File;
    use std::time::Duration;

    pub fn object_path(dev: &str) -> Result<String> {
        let name = dev.strip_prefix("/dev/").ok_or_else(|| Error::msg(format!("{dev} no es /dev/...")))?;
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(Error::msg(format!("nombre de dispositivo raro: {dev}")));
        }
        Ok(format!("/org/freedesktop/UDisks2/block_devices/{name}"))
    }

    /// Desmonta una partición por udisks2 (sin root: polkit lo permite al usuario de la sesión en discos extraíbles).
    pub fn udisks_unmount(dev: &str) -> Result<()> {
        let c = Connection::new_system().map_err(|e| Error::msg(format!("D-Bus del sistema: {e}")))?;
        let p = c.with_proxy("org.freedesktop.UDisks2", object_path(dev)?, Duration::from_secs(60));
        p.method_call::<(), _, _, _>("org.freedesktop.UDisks2.Filesystem", "Unmount", (PropMap::new(),))
            .map_err(|e| Error::msg(format!("no se pudo desmontar {dev}: {e}")))
    }

    pub fn udisks_open(dev: &str) -> Result<File> {
        let c = Connection::new_system().map_err(|e| Error::msg(format!("D-Bus del sistema: {e}")))?;
        let p = c.with_proxy("org.freedesktop.UDisks2", object_path(dev)?, Duration::from_secs(120));
        let (fd,): (dbus::arg::OwnedFd,) = p
            .method_call("org.freedesktop.UDisks2.Block", "OpenDevice", ("rw", PropMap::new()))
            .map_err(|e| Error::msg(format!("udisks2 no abrió {dev} (¿autorización denegada?): {e}")))?;
        use std::os::fd::FromRawFd;
        // dbus::arg::OwnedFd cede el descriptor; desde aquí lo cierra File
        Ok(unsafe { File::from_raw_fd(fd.into_fd()) })
    }
}

#[cfg(target_os = "linux")]
pub use linux::udisks_unmount;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_file_roundtrip_and_alignment() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("d.img");
        std::fs::File::create(&p).unwrap().set_len(1 << 20).unwrap();
        let mut d = FileDev::open_image(&p).unwrap();
        assert_eq!(d.size(), 1 << 20);
        d.write_at(512, &[7u8; 1024]).unwrap();
        let mut b = [0u8; 512];
        d.read_at(1024, &mut b).unwrap();
        assert_eq!(b, [7u8; 512]);
        assert!(d.write_at(100, &[0u8; 512]).is_err());
        assert!(d.write_at(0, &[0u8; 100]).is_err());
        assert!(d.write_at(1 << 20, &[0u8; 512]).is_err(), "fuera del disco");
        assert!(FileDev::open_image(dir.path()).is_err(), "un directorio no es una imagen");
    }
}
