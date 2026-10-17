//! Single-file local publication, not a multi-store transaction or Git synchronizer.
use super::{revision, Result};
use anyhow::{bail, Context};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
    process::Command,
};
const LIMIT: u64 = 4 * 1024 * 1024;

pub(super) fn read(path: &Path) -> Result<Vec<u8>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > LIMIT => {
            bail!("device file must be a bounded regular file")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(_) => bail!("cannot inspect device file"),
        _ => {}
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("device file limit");
    }
    Ok(bytes)
}
struct Lock(std::path::PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
/// Hold the cooperative registry lock while publishing a derived harness file.
/// This is not a power-loss transaction or a fence against arbitrary editors.
pub(super) fn with_snapshot<T>(
    path: &Path,
    before: &[u8],
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let lock_path = path.with_extension("json.lock");
    let _file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("device write locked; inspect stale lock before retry")?;
    let _lock = Lock(lock_path);
    if read(path)? != before {
        bail!("connection registry changed; preview again");
    }
    operation()
}
fn clean(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("device file requires parent directory")?;
    let probe = Command::new("git")
        .arg("-C")
        .arg(parent)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()?;
    if probe.status.success() {
        let result = Command::new("git")
            .arg("-C")
            .arg(parent)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()?;
        if !result.status.success() || !result.stdout.is_empty() {
            bail!("dirty declaration repository; resolve changes before applying");
        }
    }
    Ok(())
}
fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("device file requires parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|_| anyhow::anyhow!("cannot publish device file"))?;
    Ok(())
}
pub(super) fn apply(
    path: &Path,
    before: &[u8],
    candidate: &[u8],
    expected: Option<&str>,
) -> Result<()> {
    if candidate.len() as u64 > LIMIT {
        bail!("candidate device file exceeds storage limit");
    }
    if expected != Some(revision(before).as_str()) {
        bail!("apply requires --expected-revision from preview");
    }
    let parent = path
        .parent()
        .context("device file requires parent directory")?;
    fs::create_dir_all(parent)?;
    clean(path)?;
    let lock_path = path.with_extension("json.lock");
    let _file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .context("device write locked; inspect stale lock before retry")?;
    let _lock = Lock(lock_path);
    if read(path)? != before {
        bail!("device revision conflict; preview again");
    }
    if !before.is_empty() {
        let backup = path.with_extension("json.last-good");
        // Do not overwrite symlinks or malformed filesystem objects.
        let _ = read(&backup)?;
        publish(&backup, before)?;
    }
    if read(path)? != before {
        bail!("device revision conflict; last-good retained");
    }
    publish(path, candidate)?;
    Ok(())
}
