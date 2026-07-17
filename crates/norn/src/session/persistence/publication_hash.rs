use std::fs::File;
use std::io::{self, Read};

use sha2::{Digest as _, Sha256};

pub(super) struct HashingReader {
    inner: File,
    hasher: Sha256,
    bytes: u64,
}

impl HashingReader {
    pub(super) fn new(inner: File) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    pub(super) fn metadata_len(&self) -> io::Result<u64> {
        Ok(self.inner.metadata()?.len())
    }

    pub(super) const fn bytes_read(&self) -> u64 {
        self.bytes
    }

    pub(super) fn finish_sha256(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

impl Read for HashingReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        self.bytes = self
            .bytes
            .checked_add(u64::try_from(read).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("timeline byte count overflow"))?;
        Ok(read)
    }
}
