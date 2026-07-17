use crate::provider::usage::Usage;
use crate::session::events::SessionEvent;
use crate::session::persistence::SessionIndexEntry;

/// Exact counters represented by one active format-2 index row.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct IndexCounters {
    pub(crate) event_count: u64,
    pub(crate) total_input_tokens: u64,
    pub(crate) total_output_tokens: u64,
    pub(crate) total_cache_read_tokens: u64,
}

/// The first format-2 index field whose exact value cannot fit in `u64`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CounterOverflow {
    field: &'static str,
}

impl CounterOverflow {
    pub(crate) const fn field(self) -> &'static str {
        self.field
    }
}

impl IndexCounters {
    #[cfg(test)]
    pub(crate) fn from_entry(entry: &SessionIndexEntry) -> Self {
        Self {
            event_count: entry.event_count,
            total_input_tokens: entry.total_input_tokens,
            total_output_tokens: entry.total_output_tokens,
            total_cache_read_tokens: entry.total_cache_read_tokens,
        }
    }

    pub(crate) fn try_from_events(events: &[SessionEvent]) -> Result<Self, CounterOverflow> {
        let mut counters = Self::default();
        for event in events {
            counters.absorb(event)?;
        }
        Ok(counters)
    }

    pub(crate) fn checked_with(self, event: &SessionEvent) -> Result<Self, CounterOverflow> {
        let mut updated = self;
        updated.absorb(event)?;
        Ok(updated)
    }

    #[cfg(test)]
    pub(crate) fn checked_add(self, delta: Self) -> Result<Self, CounterOverflow> {
        Ok(Self {
            event_count: checked_add(self.event_count, delta.event_count, "event_count")?,
            total_input_tokens: checked_add(
                self.total_input_tokens,
                delta.total_input_tokens,
                "total_input_tokens",
            )?,
            total_output_tokens: checked_add(
                self.total_output_tokens,
                delta.total_output_tokens,
                "total_output_tokens",
            )?,
            total_cache_read_tokens: checked_add(
                self.total_cache_read_tokens,
                delta.total_cache_read_tokens,
                "total_cache_read_tokens",
            )?,
        })
    }

    pub(crate) fn absorb(&mut self, event: &SessionEvent) -> Result<(), CounterOverflow> {
        self.event_count = checked_add(self.event_count, 1, "event_count")?;
        let SessionEvent::AssistantMessage { usage, .. } = event else {
            return Ok(());
        };
        self.total_input_tokens = checked_add(
            self.total_input_tokens,
            usage.input_tokens,
            "total_input_tokens",
        )?;
        self.total_output_tokens = checked_add(
            self.total_output_tokens,
            usage.output_tokens,
            "total_output_tokens",
        )?;
        self.total_cache_read_tokens = checked_add(
            self.total_cache_read_tokens,
            usage.cache_read_tokens,
            "total_cache_read_tokens",
        )?;
        Ok(())
    }

    pub(crate) fn tracked_usage(self) -> Usage {
        Usage {
            input_tokens: self.total_input_tokens,
            output_tokens: self.total_output_tokens,
            cache_read_tokens: self.total_cache_read_tokens,
            ..Usage::default()
        }
    }

    pub(crate) fn apply_to(self, entry: &mut SessionIndexEntry) {
        entry.event_count = self.event_count;
        entry.total_input_tokens = self.total_input_tokens;
        entry.total_output_tokens = self.total_output_tokens;
        entry.total_cache_read_tokens = self.total_cache_read_tokens;
    }
}

fn checked_add(current: u64, delta: u64, field: &'static str) -> Result<u64, CounterOverflow> {
    current.checked_add(delta).ok_or(CounterOverflow { field })
}
