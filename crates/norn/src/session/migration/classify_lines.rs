use std::io::BufRead;

pub(super) struct PhysicalLine {
    pub(super) number: u64,
    pub(super) bytes: Vec<u8>,
    pub(super) terminated: bool,
}

pub(super) struct PhysicalLines<R> {
    reader: R,
    next_number: u64,
}

impl<R: BufRead> PhysicalLines<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader,
            next_number: 1,
        }
    }

    pub(super) fn next_line(&mut self) -> Result<Option<PhysicalLine>, std::io::Error> {
        let mut bytes = Vec::new();
        if self.reader.read_until(b'\n', &mut bytes)? == 0 {
            return Ok(None);
        }
        let terminated = bytes.last().is_some_and(|byte| *byte == b'\n');
        if terminated {
            bytes.pop();
        }
        let number = self.next_number;
        self.next_number = self.next_number.checked_add(1).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "JSONL line number exceeds the migration manifest representation",
            )
        })?;
        Ok(Some(PhysicalLine {
            number,
            bytes,
            terminated,
        }))
    }
}
