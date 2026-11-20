//! The small file operations every write in this crate uses.

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use cm_core::AbsPath;

/// The mode of a file the tool creates in an agent's tree, before the umask.
pub const NEW_FILE_MODE: u32 = 0o644;
/// The mode of every file the tool keeps in its own data directory.
pub const DATA_FILE_MODE: u32 = 0o600;
const DATA_DIR_MODE: u32 = 0o700;

pub fn io_detail(err: io::Error) -> cm_core::ApiError {
    cm_core::ApiError::Io {
        detail: err.to_string(),
    }
}

/// Creates `dir` and its parents with mode 0700. An existing directory is left as it is.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(DATA_DIR_MODE)
        .create(dir)
}

/// Writes `bytes` to `tmp`, syncs it, sets `mode`, renames it over `target` and syncs the
/// directory. `tmp` must be in the directory of `target` so the rename stays on one filesystem.
/// A failure removes `tmp`.
pub fn atomic_write(target: &Path, tmp: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let _ = fs::remove_file(tmp);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(tmp)?;
        file.write_all(bytes)?;
        file.set_permissions(Permissions::from_mode(mode))?;
        file.sync_all()?;
        fs::rename(tmp, target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
        return result;
    }
    if let Some(parent) = target.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Writes a file of the data directory: temp file, sync, rename, mode 0600.
pub fn write_data_file(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        create_private_dir(parent)?;
    }
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!(".{name}.tmp"));
    atomic_write(target, &tmp, bytes, DATA_FILE_MODE)
}

/// Where a write to `path` lands: the target of a symlink, else `path` itself. A symlink to
/// nothing resolves to itself: the tool does not create a file through a link (design section 4).
pub fn write_target(path: &AbsPath) -> PathBuf {
    let plain = path.as_path();
    match fs::symlink_metadata(plain) {
        Ok(meta) if meta.is_symlink() => {
            fs::canonicalize(plain).unwrap_or_else(|_| plain.to_path_buf())
        }
        _ => plain.to_path_buf(),
    }
}

/// The mode bits of an existing file, or `None`.
pub fn mode_of(path: &Path) -> Option<u32> {
    fs::metadata(path)
        .ok()
        .map(|meta| meta.permissions().mode() & 0o7777)
}
