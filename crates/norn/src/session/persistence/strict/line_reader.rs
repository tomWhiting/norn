use std::io::BufRead;
use std::path::{Path, PathBuf};

use super::StrictStoreError;

pub(super) struct CompleteLines<R> {
    reader: R,
    path: PathBuf,
    next_line: usize,
}

impl<R: BufRead> CompleteLines<R> {
    pub(super) fn new(reader: R, path: &Path) -> Self {
        Self {
            reader,
            path: path.to_path_buf(),
            next_line: 1,
        }
    }

    pub(super) fn next(&mut self) -> Result<Option<(usize, Vec<u8>)>, StrictStoreError> {
        let line_number = self.next_line;
        let mut raw = Vec::new();
        let read = self
            .reader
            .read_until(b'\n', &mut raw)
            .map_err(|error| StrictStoreError::io(&self.path, error))?;
        if read == 0 {
            return Ok(None);
        }
        self.next_line = self.next_line.saturating_add(1);
        if raw.last() != Some(&b'\n') {
            return Err(StrictStoreError::TornTail {
                path: self.path.clone(),
                line: line_number,
            });
        }
        raw.pop();
        if raw.last() == Some(&b'\r') {
            raw.pop();
        }
        if raw.iter().all(u8::is_ascii_whitespace) {
            return Err(StrictStoreError::EmptyRow {
                path: self.path.clone(),
                line: line_number,
            });
        }
        Ok(Some((line_number, raw)))
    }
}
