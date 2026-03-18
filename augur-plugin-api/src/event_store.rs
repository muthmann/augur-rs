use std::mem;

use crate::FfiCdEvent;

const DEFAULT_MEMORY_BUDGET_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBoundary {
    pub start_index: usize,
    pub end_index: usize,
    pub start_timestamp_us: u64,
    pub end_timestamp_us: u64,
}

#[derive(Debug, Clone)]
pub struct EventStore {
    events: Vec<FfiCdEvent>,
    boundaries: Vec<FrameBoundary>,
    memory_budget_bytes: usize,
}

impl Default for EventStore {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            boundaries: Vec::new(),
            memory_budget_bytes: DEFAULT_MEMORY_BUDGET_BYTES,
        }
    }
}

impl EventStore {
    pub fn push_frame(
        &mut self,
        events: &[FfiCdEvent],
        start_timestamp_us: u64,
        end_timestamp_us: u64,
    ) {
        let start_index = self.events.len();
        self.events.extend_from_slice(events);
        let end_index = self.events.len();
        self.boundaries.push(FrameBoundary {
            start_index,
            end_index,
            start_timestamp_us,
            end_timestamp_us,
        });
        self.enforce_memory_budget();
    }

    pub fn events_in_range(&self, start_timestamp_us: u64, end_timestamp_us: u64) -> &[FfiCdEvent] {
        if self.events.is_empty() || start_timestamp_us > end_timestamp_us {
            return &[];
        }

        let first_boundary = self
            .boundaries
            .partition_point(|boundary| boundary.end_timestamp_us < start_timestamp_us);
        let last_boundary_exclusive = self
            .boundaries
            .partition_point(|boundary| boundary.start_timestamp_us <= end_timestamp_us);

        if first_boundary >= last_boundary_exclusive {
            return &[];
        }

        let start_index = self.boundaries[first_boundary].start_index;
        let end_index = self.boundaries[last_boundary_exclusive - 1].end_index;
        let candidate = &self.events[start_index..end_index];

        let event_start = candidate.partition_point(|event| event.timestamp < start_timestamp_us);
        let event_end = candidate.partition_point(|event| event.timestamp <= end_timestamp_us);
        &candidate[event_start..event_end]
    }

    pub fn all_events(&self) -> &[FfiCdEvent] {
        &self.events
    }

    pub fn oldest_timestamp_us(&self) -> Option<u64> {
        self.boundaries
            .first()
            .map(|boundary| boundary.start_timestamp_us)
    }

    pub fn frame_count(&self) -> usize {
        self.boundaries.len()
    }

    pub fn frame_boundaries(&self) -> &[FrameBoundary] {
        &self.boundaries
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.boundaries.clear();
    }

    pub fn set_memory_budget(&mut self, memory_budget_bytes: usize) {
        self.memory_budget_bytes = memory_budget_bytes;
        self.enforce_memory_budget();
    }

    pub fn memory_budget_bytes(&self) -> usize {
        self.memory_budget_bytes
    }

    pub fn memory_usage_bytes(&self) -> usize {
        self.events.len() * mem::size_of::<FfiCdEvent>()
    }

    fn enforce_memory_budget(&mut self) {
        if self.boundaries.len() <= 1 {
            return;
        }

        let event_size = mem::size_of::<FfiCdEvent>();
        let mut trim_events = 0usize;
        let mut trim_frames = 0usize;
        let mut remaining_bytes = self.memory_usage_bytes();

        while self.boundaries.len().saturating_sub(trim_frames) > 1
            && remaining_bytes > self.memory_budget_bytes
        {
            let boundary = self.boundaries[trim_frames];
            trim_events = boundary.end_index;
            remaining_bytes = remaining_bytes
                .saturating_sub((boundary.end_index - boundary.start_index) * event_size);
            trim_frames += 1;
        }

        if trim_frames == 0 {
            return;
        }

        self.events.drain(..trim_events);
        self.boundaries.drain(..trim_frames);
        for boundary in &mut self.boundaries {
            boundary.start_index -= trim_events;
            boundary.end_index -= trim_events;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EventStore, FrameBoundary};
    use crate::FfiCdEvent;

    fn event(timestamp: u64, x: u16) -> FfiCdEvent {
        FfiCdEvent {
            timestamp,
            x,
            y: 0,
            polarity: 1,
        }
    }

    #[test]
    fn push_frame_tracks_boundaries_and_range_queries() {
        let mut store = EventStore::default();
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3), event(40, 4)], 30, 40);

        assert_eq!(store.frame_count(), 2);
        assert_eq!(store.oldest_timestamp_us(), Some(10));
        assert_eq!(
            store.frame_boundaries(),
            &[
                FrameBoundary {
                    start_index: 0,
                    end_index: 2,
                    start_timestamp_us: 10,
                    end_timestamp_us: 20,
                },
                FrameBoundary {
                    start_index: 2,
                    end_index: 4,
                    start_timestamp_us: 30,
                    end_timestamp_us: 40,
                },
            ]
        );
        assert_eq!(store.events_in_range(15, 35), &[event(20, 2), event(30, 3)]);
    }

    #[test]
    fn lowering_budget_evicts_oldest_complete_frames() {
        let mut store = EventStore::default();
        let event_size = std::mem::size_of::<FfiCdEvent>();

        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3), event(40, 4)], 30, 40);
        store.set_memory_budget(event_size * 3);

        assert_eq!(store.frame_count(), 1);
        assert_eq!(store.oldest_timestamp_us(), Some(30));
        assert_eq!(store.all_events(), &[event(30, 3), event(40, 4)]);
        assert_eq!(
            store.frame_boundaries(),
            &[FrameBoundary {
                start_index: 0,
                end_index: 2,
                start_timestamp_us: 30,
                end_timestamp_us: 40,
            }]
        );
    }

    #[test]
    fn oversized_latest_frame_is_retained() {
        let mut store = EventStore::default();
        let event_size = std::mem::size_of::<FfiCdEvent>();

        store.set_memory_budget(event_size);
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);

        assert_eq!(store.frame_count(), 1);
        assert_eq!(store.all_events().len(), 2);
    }
}
