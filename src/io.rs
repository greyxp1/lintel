//! File-descriptor write helpers.

use rustix::fd::{AsFd, BorrowedFd};
use rustix::io::{Errno, write};
use wllib::error::SysError;

/// Writes `msg` to the file descriptor. Retries on partial write and `EINTR` until all bytes have
/// been written or a real error occurs.
fn write_fd<Fd: AsFd>(fd: Fd, msg: impl AsRef<[u8]>) -> Result<(), SysError> {
  let mut msg = msg.as_ref();
  while !msg.is_empty() {
    let res = match write(fd.as_fd(), msg) {
      Ok(0) => break,
      Ok(n) => n,
      Err(Errno::INTR) => continue,
      Err(err) => return Err(SysError::from_errno("write", err)),
    };
    msg = &msg[res..];
  }
  Ok(())
}

/// Writes the buffer to stderr.
pub fn write_stderr(msg: impl AsRef<[u8]>) {
  let fd = unsafe { BorrowedFd::borrow_raw(2) };
  let _ = write_fd(fd, msg);
}

#[cfg(feature = "standalone")]
pub fn write_stdout(msg: impl AsRef<[u8]>) {
  let fd = unsafe { BorrowedFd::borrow_raw(1) };
  let _ = write_fd(fd, msg);
}
