//! Private filesystem primitives for persisted session data.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
#[cfg(unix)]
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Create `path` and missing parents without granting group or world access.
///
/// Unix applies `0700` at inode creation and hardens the requested directory
/// through a no-follow descriptor. Other platforms retain `create_dir_all`'s
/// existing behavior because their permission models do not expose equivalent
/// Unix mode and `O_NOFOLLOW` guarantees through this implementation.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    create_private_dir_all_impl(path)
}

/// Open an existing private regular file for reading and harden legacy modes.
pub(crate) fn open_private_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    open_private_file(&mut options, path)
}

/// Open an existing private regular file for append and legacy repair.
pub(crate) fn open_private_read_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).append(true);
    open_private_file(&mut options, path)
}

/// Open or create a private regular append-only file.
pub(crate) fn open_private_append_create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    open_private_file(&mut options, path)
}

/// Open or create a private regular file used solely for advisory locking.
pub(crate) fn open_private_lock(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    open_private_file(&mut options, path)
}

/// Exclusively create a private regular file.
pub(crate) fn create_private_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    open_private_file(&mut options, path)
}

/// Create or truncate a spool payload without following a Unix symlink or
/// accepting a non-regular target.
///
/// On non-Unix platforms this deliberately retains `File::create` semantics.
pub(crate) fn create_private_spool(path: &Path) -> io::Result<File> {
    create_private_spool_impl(path)
}

#[cfg(unix)]
fn non_regular_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "session persistence targets must be regular files without symlinks",
    )
}

#[cfg(unix)]
fn configure_private_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options
        .mode(PRIVATE_FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
}

#[cfg(not(unix))]
fn configure_private_file(_options: &mut OpenOptions) {}

fn open_private_file(options: &mut OpenOptions, path: &Path) -> io::Result<File> {
    configure_private_file(options);
    let file = options.open(path)?;
    harden_regular_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn harden_regular_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    if !file.metadata()?.file_type().is_file() {
        return Err(non_regular_error());
    }
    file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

#[cfg(not(unix))]
fn harden_regular_file(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn create_private_dir_all_impl(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let existed = match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(PRIVATE_DIRECTORY_MODE);
    builder.create(path)?;

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path)?;
    if !directory.metadata()?.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "session persistence directories must not be symlinks",
        ));
    }
    let mode = if existed {
        // Tightening must never add owner permissions that an operator
        // deliberately removed (for example a read-only session directory).
        directory.metadata()?.permissions().mode() & PRIVATE_DIRECTORY_MODE
    } else {
        PRIVATE_DIRECTORY_MODE
    };
    directory.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn create_private_dir_all_impl(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn create_private_spool_impl(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    open_private_file(&mut options, path)
}

#[cfg(not(unix))]
fn create_private_spool_impl(path: &Path) -> io::Result<File> {
    File::create(path)
}
