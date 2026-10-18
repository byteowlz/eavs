use super::{revision, MAX_BYTES};
use anyhow::{bail, Result};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    match fs::symlink_metadata(path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() || m.len() > MAX_BYTES as u64 => {
            bail!("portable file must be bounded and regular")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => bail!("cannot inspect portable file"),
        _ => {}
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        bail!("portable file exceeds limit");
    }
    Ok(bytes)
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("parent required"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| anyhow::anyhow!("portable publication failed"))?;
    Ok(())
}
pub(super) fn apply(
    path: &Path,
    candidate: &[u8],
    expected: &str,
    validate: impl Fn(&[u8]) -> Result<()>,
) -> Result<()> {
    validate(candidate)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("parent required"))?;
    fs::create_dir_all(parent)?;
    let lockpath = path.with_extension("portable.lock");
    let _file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lockpath)?;
    let _lock = Lock(lockpath);
    let before = read(path)?;
    if revision(&before) != expected {
        bail!("portable revision conflict; preview again");
    }
    if path.exists() {
        validate(&before)?;
        let backup = path.with_extension("portable.last-good");
        read(&backup)?;
        publish(&backup, &before)?;
    }
    if read(path)? != before {
        bail!("portable revision conflict; last-good retained");
    }
    publish(path, candidate)
}
