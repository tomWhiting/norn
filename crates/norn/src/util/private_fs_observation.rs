//! Non-mutating descriptor-relative reads for local inspection commands.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
/// Open an existing regular file without creating entries or changing modes.
pub(super) fn open_regular_file(root: &Path, relative: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let mut directory =
        open(Path::new("/"), super::directory_flags(), Mode::empty()).map_err(io::Error::from)?;
    for component in super::absolute_components(root)? {
        directory = openat(
            &directory,
            &component,
            super::directory_flags(),
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
    }

    let (parent, name) = super::split_file_path(relative)?;
    for component in super::relative_components(&parent, true)? {
        directory = openat(
            &directory,
            component,
            super::directory_flags(),
            Mode::empty(),
        )
        .map_err(io::Error::from)?;
    }

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let descriptor = openat(&directory, name, flags, Mode::empty()).map_err(io::Error::from)?;
    let file = File::from(descriptor);
    if !file.metadata()?.file_type().is_file() {
        return Err(super::non_regular());
    }
    Ok(file)
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
/// Fail closed where descriptor-relative observation is unavailable.
pub(super) fn open_regular_file(_: &Path, _: &Path) -> io::Result<File> {
    Err(super::unsupported())
}
