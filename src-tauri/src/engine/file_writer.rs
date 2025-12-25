use std::fs::File;
use std::io;

#[cfg(windows)]
fn write_at_impl(file: &File, offset: u64, buf: &[u8]) -> io::Result<usize> {
  use std::os::windows::fs::FileExt;
  file.seek_write(buf, offset)
}

#[cfg(unix)]
fn write_at_impl(file: &File, offset: u64, buf: &[u8]) -> io::Result<usize> {
  use std::os::unix::fs::FileExt;
  file.write_at(buf, offset)
}

pub fn write_at_all(file: &File, mut offset: u64, mut buf: &[u8]) -> io::Result<()> {
  while !buf.is_empty() {
    let n = write_at_impl(file, offset, buf)?;
    if n == 0 {
      return Err(io::Error::new(
        io::ErrorKind::WriteZero,
        "failed to write to file",
      ));
    }
    offset += n as u64;
    buf = &buf[n..];
  }
  Ok(())
}


