use std::path::{Path, PathBuf};

use super::SessionMigrationError;

pub(in crate::session::migration) fn encode_relative_path(
    path: &Path,
) -> Result<String, SessionMigrationError> {
    if let Some(value) = path.to_str() {
        if value.starts_with("unix-path-hex:") || value.starts_with("utf8-path-hex:") {
            return Ok(format!("utf8-path-hex:{}", hex_encode(value.as_bytes())?));
        }
        return Ok(value.to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let bytes = path.as_os_str().as_bytes();
        Ok(format!("unix-path-hex:{}", hex_encode(bytes)?))
    }
    #[cfg(not(unix))]
    Err(SessionMigrationError::UnrepresentableSource {
        reason: format!("source path is not valid UTF-8: {}", path.display()),
    })
}

pub(in crate::session::migration) fn decode_relative_path(
    selector: &str,
) -> Result<PathBuf, SessionMigrationError> {
    if let Some(encoded) = selector.strip_prefix("utf8-path-hex:") {
        let bytes = hex_decode(encoded)?;
        let value = String::from_utf8(bytes).map_err(|error| {
            SessionMigrationError::UnrepresentableSource {
                reason: format!("UTF-8 migration path selector is invalid: {error}"),
            }
        })?;
        return Ok(PathBuf::from(value));
    }
    if let Some(encoded) = selector.strip_prefix("unix-path-hex:") {
        let bytes = hex_decode(encoded)?;
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;

            return Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)));
        }
        #[cfg(not(unix))]
        {
            let _ = bytes;
            return Err(SessionMigrationError::UnrepresentableSource {
                reason: "Unix migration path selector is unsupported on this platform".to_owned(),
            });
        }
    }
    Ok(PathBuf::from(selector))
}

fn hex_encode(bytes: &[u8]) -> Result<String, SessionMigrationError> {
    let capacity =
        bytes
            .len()
            .checked_mul(2)
            .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
                reason: "migration path selector exceeds the string representation".to_owned(),
            })?;
    let mut encoded = String::with_capacity(capacity);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").map_err(|error| {
            SessionMigrationError::UnrepresentableSource {
                reason: error.to_string(),
            }
        })?;
    }
    Ok(encoded)
}

fn hex_decode(encoded: &str) -> Result<Vec<u8>, SessionMigrationError> {
    if !encoded.len().is_multiple_of(2) {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: "migration path selector contains an odd number of hex digits".to_owned(),
        });
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Result<u8, SessionMigrationError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(SessionMigrationError::UnrepresentableSource {
            reason: "migration path selector contains non-lowercase-hex data".to_owned(),
        }),
    }
}
