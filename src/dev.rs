use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;

use nix::fcntl::OFlag;
use nix::sys::stat::Mode;
use tokio::io::unix::AsyncFd;

/// An async wrapper around a file descriptor to allow nonblocking reading.
///
/// Unfortunately, tokio::fs uses spawn_blocking which causes problems when reading from files that
/// aren't real files (such as /dev/hidraw*) because it ends up blocking indefinitely if there is no
/// data available. This results in a hang when shutting down the runtime, thus the need for this
/// wrapper.
pub struct DeviceFile {
    fd: AsyncFd<OwnedFd>,
}

impl DeviceFile {
    pub fn open(path: impl AsRef<Path>) -> io::Result<DeviceFile> {
        let fd = nix::fcntl::open(
            path.as_ref(),
            OFlag::O_NONBLOCK | OFlag::O_CLOEXEC | OFlag::O_RDONLY,
            Mode::empty(),
        )?;

        Ok(DeviceFile {
            fd: AsyncFd::new(fd)?,
        })
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let result = self.fd.readable().await?.try_io(|fd| {
                let fd = fd.get_ref();
                match nix::unistd::read(fd, buf) {
                    Ok(bytes) => Ok(bytes),
                    Err(errno) => Err(io::Error::from_raw_os_error(errno as i32)),
                }
            });

            match result {
                Ok(result) => return result,
                Err(_) => continue,
            };
        }
    }
}
