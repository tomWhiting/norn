//! Stable identity checks for lazily reopened private regular files.

use std::fs::File;
use std::io;

/// Device/inode identity captured when a private file is first created/opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrivateFileIdentity {
    device: u64,
    inode: u64,
}

impl PrivateFileIdentity {
    /// Capture the current regular file's stable identity.
    pub(crate) fn capture(file: &File) -> io::Result<Self> {
        capture(file)
    }

    /// Reject a reopened file when its inode changed since construction.
    pub(crate) fn verify(self, file: &File) -> io::Result<()> {
        if Self::capture(file)? == self {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private storage file changed identity after it was opened",
        ))
    }
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
fn capture(file: &File) -> io::Result<PrivateFileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    Ok(PrivateFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(any(not(unix), target_os = "redox", target_os = "espidf"))]
fn capture(_file: &File) -> io::Result<PrivateFileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private file identity requires a supported descriptor-capable Unix target",
    ))
}
