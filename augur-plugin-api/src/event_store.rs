use std::collections::VecDeque;

use crate::{FfiCdEvent, FfiEventFrame};

const DEFAULT_MEMORY_BUDGET_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
struct StoredFrame {
    events: Box<[FfiCdEvent]>,
    window_start_us: u64,
    window_end_us: u64,
    byte_len: usize,
    event_count: usize,
}

impl StoredFrame {
    fn new(events: &[FfiCdEvent], window_start_us: u64, window_end_us: u64) -> Self {
        let events = events.to_vec().into_boxed_slice();
        let event_count = events.len();
        let byte_len = event_count * std::mem::size_of::<FfiCdEvent>();
        Self {
            events,
            window_start_us,
            window_end_us,
            byte_len,
            event_count,
        }
    }

    fn as_ffi_frame(&self) -> FfiEventFrame {
        FfiEventFrame::from_slice(&self.events, self.window_start_us, self.window_end_us)
    }
}

#[derive(Debug, Clone)]
pub struct EventStore {
    frames: VecDeque<StoredFrame>,
    memory_budget_bytes: usize,
    memory_usage_bytes: usize,
}

impl Default for EventStore {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
            memory_budget_bytes: DEFAULT_MEMORY_BUDGET_BYTES,
            memory_usage_bytes: 0,
        }
    }
}

impl EventStore {
    pub fn push_frame(&mut self, events: &[FfiCdEvent], window_start_us: u64, window_end_us: u64) {
        if events.is_empty() {
            return;
        }
        let frame = StoredFrame::new(events, window_start_us, window_end_us);
        self.memory_usage_bytes += frame.byte_len;
        self.frames.push_back(frame);
        self.enforce_memory_budget();
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn oldest_timestamp_us(&self) -> Option<u64> {
        self.frames.front().map(|frame| frame.window_start_us)
    }

    pub fn frame(&self, index: usize) -> Option<FfiEventFrame> {
        self.frames.get(index).map(StoredFrame::as_ffi_frame)
    }

    pub fn frames(&self) -> Vec<FfiEventFrame> {
        self.frames.iter().map(StoredFrame::as_ffi_frame).collect()
    }

    pub fn frame_range_for_timestamps(
        &self,
        start_timestamp_us: u64,
        end_timestamp_us: u64,
    ) -> Option<(usize, usize)> {
        if self.frames.is_empty() || start_timestamp_us > end_timestamp_us {
            return None;
        }

        let start_index = self.first_frame_with_window_end_at_or_after(start_timestamp_us)?;
        let end_index = self.first_frame_with_window_start_after(end_timestamp_us);
        if start_index >= end_index {
            None
        } else {
            Some((start_index, end_index))
        }
    }

    pub fn frames_in_range(
        &self,
        start_timestamp_us: u64,
        end_timestamp_us: u64,
    ) -> Vec<FfiEventFrame> {
        let Some((start_index, end_index)) =
            self.frame_range_for_timestamps(start_timestamp_us, end_timestamp_us)
        else {
            return Vec::new();
        };

        let mut frames = Vec::with_capacity(end_index.saturating_sub(start_index));
        for index in start_index..end_index {
            if let Some(frame) = self.frame(index) {
                frames.push(frame);
            }
        }
        frames
    }

    pub fn collect_events_in_range(
        &self,
        start_timestamp_us: u64,
        end_timestamp_us: u64,
        out: &mut Vec<FfiCdEvent>,
    ) {
        out.clear();
        let Some((start_index, end_index)) =
            self.frame_range_for_timestamps(start_timestamp_us, end_timestamp_us)
        else {
            return;
        };

        let mut total_events = 0usize;
        for index in start_index..end_index {
            total_events += self.frames[index].event_count;
        }
        out.reserve(total_events);

        for index in start_index..end_index {
            let frame = &self.frames[index];
            out.extend_from_slice(augur_event_types::inclusive_window(
                &frame.events,
                start_timestamp_us,
                end_timestamp_us,
            ));
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.memory_usage_bytes = 0;
    }

    pub fn set_memory_budget(&mut self, memory_budget_bytes: usize) {
        self.memory_budget_bytes = memory_budget_bytes;
        self.enforce_memory_budget();
    }

    pub fn memory_budget_bytes(&self) -> usize {
        self.memory_budget_bytes
    }

    pub fn memory_usage_bytes(&self) -> usize {
        self.memory_usage_bytes
    }

    fn first_frame_with_window_end_at_or_after(&self, timestamp_us: u64) -> Option<usize> {
        let mut left = 0usize;
        let mut right = self.frames.len();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.frames[mid].window_end_us < timestamp_us {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        (left < self.frames.len()).then_some(left)
    }

    fn first_frame_with_window_start_after(&self, timestamp_us: u64) -> usize {
        let mut left = 0usize;
        let mut right = self.frames.len();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.frames[mid].window_start_us <= timestamp_us {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        left
    }

    fn enforce_memory_budget(&mut self) {
        while self.frames.len() > 1 && self.memory_usage_bytes > self.memory_budget_bytes {
            let Some(frame) = self.frames.pop_front() else {
                break;
            };
            self.memory_usage_bytes = self.memory_usage_bytes.saturating_sub(frame.byte_len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EventStore;
    use crate::FfiCdEvent;

    fn event(timestamp: u64, x: u16) -> FfiCdEvent {
        FfiCdEvent::new(x, 0, timestamp, 1)
    }

    #[test]
    fn push_frame_tracks_frame_windows_and_range_queries() {
        let mut store = EventStore::default();
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3), event(40, 4)], 30, 40);

        assert_eq!(store.frame_count(), 2);
        assert_eq!(store.oldest_timestamp_us(), Some(10));

        let first_frame = store.frame(0).expect("first frame");
        let second_frame = store.frame(1).expect("second frame");
        assert_eq!(
            unsafe { first_frame.as_slice() },
            &[event(10, 1), event(20, 2)]
        );
        assert_eq!(
            unsafe { second_frame.as_slice() },
            &[event(30, 3), event(40, 4)]
        );

        assert_eq!(store.frame_range_for_timestamps(15, 35), Some((0, 2)));
        assert_eq!(store.frames_in_range(15, 35).len(), 2);

        let frames = store.frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            unsafe { frames[0].as_slice() },
            &[event(10, 1), event(20, 2)]
        );
        assert_eq!(
            unsafe { frames[1].as_slice() },
            &[event(30, 3), event(40, 4)]
        );

        let mut flattened = Vec::new();
        store.collect_events_in_range(15, 35, &mut flattened);
        assert_eq!(flattened, &[event(20, 2), event(30, 3)]);
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
        assert_eq!(store.memory_usage_bytes(), event_size * 2);
        assert_eq!(
            unsafe { store.frame(0).expect("remaining frame").as_slice() },
            &[event(30, 3), event(40, 4)]
        );
    }

    #[test]
    fn oversized_latest_frame_is_retained() {
        let mut store = EventStore::default();
        let event_size = std::mem::size_of::<FfiCdEvent>();

        store.set_memory_budget(event_size);
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);

        assert_eq!(store.frame_count(), 1);
        assert_eq!(store.memory_usage_bytes(), event_size * 2);
        assert_eq!(
            unsafe { store.frame(0).expect("remaining frame").as_slice() },
            &[event(10, 1), event(20, 2)]
        );
    }

    #[test]
    fn push_frame_ignores_empty_event_batches() {
        let mut store = EventStore::default();
        store.push_frame(&[], 10, 20);

        assert_eq!(store.frame_count(), 0);
        assert_eq!(store.memory_usage_bytes(), 0);
        assert_eq!(store.oldest_timestamp_us(), None);
    }
}
