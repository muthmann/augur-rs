use std::{
    collections::{HashMap, VecDeque},
    fmt,
    ops::Range,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CompactEvent {
    pub x: u16,
    pub y: u16,
    pub polarity: u8,
    pub _pad: [u8; 3],
    pub t_us: i64,
}

impl CompactEvent {
    pub const fn new(x: u16, y: u16, timestamp_us: u64, polarity: u8) -> Self {
        Self {
            x,
            y,
            polarity,
            _pad: [0; 3],
            t_us: if timestamp_us > i64::MAX as u64 {
                i64::MAX
            } else {
                timestamp_us as i64
            },
        }
    }

    pub const fn with_signed_time(x: u16, y: u16, t_us: i64, polarity: u8) -> Self {
        Self {
            x,
            y,
            polarity,
            _pad: [0; 3],
            t_us,
        }
    }

    pub const fn timestamp_us(self) -> u64 {
        if self.t_us < 0 {
            0
        } else {
            self.t_us as u64
        }
    }

    pub const fn is_on(self) -> bool {
        self.polarity != 0
    }
}

/// Returns the sub-slice of an ascending-timestamp event slice whose timestamps
/// fall within the inclusive window `[start_us, end_us]`.
///
/// Both bounds are resolved with `partition_point`, so the input must already be
/// sorted by `timestamp_us()` (which holds for every frame stored in the ring).
pub fn inclusive_window(events: &[CompactEvent], start_us: u64, end_us: u64) -> &[CompactEvent] {
    let start = events.partition_point(|event| event.timestamp_us() < start_us);
    let end = events.partition_point(|event| event.timestamp_us() <= end_us);
    &events[start..end]
}

/// An EVT3 `EXT_TRIGGER` edge on the camera clock.
///
/// The layout is a fixed 16-byte `#[repr(C)]` record so the same type can
/// cross the dynamic plugin ABI unchanged. `level` is stored as a raw byte
/// (0 = falling, 1 = rising); use [`ExternalTriggerEvent::is_rising`] for the
/// ergonomic view.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ExternalTriggerEvent {
    /// Camera-clock timestamp, unwrapped across the 24-bit EVT3 rollover.
    pub timestamp_us: u64,
    /// EVT3 trigger channel id.
    pub id: u8,
    /// Edge level: 0 = falling, 1 = rising.
    pub level: u8,
    pub _reserved: [u8; 6],
}

impl ExternalTriggerEvent {
    pub const fn new(timestamp_us: u64, id: u8, rising: bool) -> Self {
        Self {
            timestamp_us,
            id,
            level: rising as u8,
            _reserved: [0; 6],
        }
    }

    pub const fn is_rising(self) -> bool {
        self.level != 0
    }
}

/// Returns the sub-slice of an ascending-timestamp trigger slice whose
/// timestamps fall within the inclusive window `[start_us, end_us]`.
pub fn inclusive_trigger_window(
    triggers: &[ExternalTriggerEvent],
    start_us: u64,
    end_us: u64,
) -> &[ExternalTriggerEvent] {
    let start = triggers.partition_point(|trigger| trigger.timestamp_us < start_us);
    let end = triggers.partition_point(|trigger| trigger.timestamp_us <= end_us);
    &triggers[start..end]
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CursorId(u64);

impl CursorId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FrameWindowEntry {
    pub window_start_us: u64,
    pub window_end_us: u64,
    pub first_event_idx: u64,
    pub event_count: u32,
    pub physical_start: usize,
    pub generation: u32,
}

impl FrameWindowEntry {
    pub fn end_event_idx(&self) -> u64 {
        self.first_event_idx + u64::from(self.event_count)
    }

    fn physical_range(&self) -> Range<usize> {
        self.physical_start..self.physical_start + self.event_count as usize
    }

    fn overlaps_physical(&self, range: Range<usize>) -> bool {
        let own = self.physical_range();
        own.start < range.end && range.start < own.end
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameIndex {
    entries: VecDeque<FrameWindowEntry>,
}

impl FrameIndex {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> impl Iterator<Item = &FrameWindowEntry> {
        self.entries.iter()
    }

    pub fn get(&self, index: usize) -> Option<&FrameWindowEntry> {
        self.entries.get(index)
    }

    pub fn retained_event_range(&self) -> Option<Range<u64>> {
        let first = self.entries.front()?;
        let last = self.entries.back()?;
        Some(first.first_event_idx..last.end_event_idx())
    }

    pub fn entry_containing_event(&self, event_idx: u64) -> Option<&FrameWindowEntry> {
        let mut left = 0usize;
        let mut right = self.entries.len();
        while left < right {
            let mid = left + (right - left) / 2;
            let entry = &self.entries[mid];
            if event_idx < entry.first_event_idx {
                right = mid;
            } else if event_idx >= entry.end_event_idx() {
                left = mid + 1;
            } else {
                return Some(entry);
            }
        }
        None
    }

    fn push_back(&mut self, entry: FrameWindowEntry) {
        self.entries.push_back(entry);
    }

    fn pop_front(&mut self) -> Option<FrameWindowEntry> {
        self.entries.pop_front()
    }
}

#[derive(Clone, Debug)]
pub struct EventChunk {
    pub events: Vec<CompactEvent>,
    /// External trigger edges inside `[start_us, end_us]`. Sources that do
    /// not carry triggers (decoded imports, the live ring) leave this empty.
    pub triggers: Vec<ExternalTriggerEvent>,
    pub start_us: u64,
    pub end_us: u64,
}

pub trait EventSource {
    fn fetch_range(&self, start_us: u64, end_us: u64) -> Result<EventChunk, FetchError>;
}

#[derive(Debug)]
pub enum FetchError {
    OutOfTimeline,
    Io(std::io::Error),
    Decode(String),
    Cancelled,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfTimeline => f.write_str("requested range is outside the event timeline"),
            Self::Io(err) => write!(f, "event source I/O failed: {err}"),
            Self::Decode(err) => write!(f, "event source decode failed: {err}"),
            Self::Cancelled => f.write_str("event source request was cancelled"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::OutOfTimeline | Self::Decode(_) | Self::Cancelled => None,
        }
    }
}

#[derive(Clone)]
pub enum BackpressureBehavior {
    BlockWriter { max_block_us: u32 },
    SpillToOverflowSource(Arc<dyn EventSource + Send + Sync>),
    FailLoud,
}

impl fmt::Debug for BackpressureBehavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockWriter { max_block_us } => f
                .debug_struct("BlockWriter")
                .field("max_block_us", max_block_us)
                .finish(),
            Self::SpillToOverflowSource(_) => f.write_str("SpillToOverflowSource(..)"),
            Self::FailLoud => f.write_str("FailLoud"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum CursorPolicy {
    Lossless { backpressure: BackpressureBehavior },
    BestEffort,
}

#[derive(Debug)]
pub struct ConsumerCursor {
    id: CursorId,
    next_event_idx: AtomicU64,
    policy: CursorPolicy,
    label: String,
}

impl ConsumerCursor {
    fn new(id: CursorId, next_event_idx: u64, policy: CursorPolicy, label: String) -> Self {
        Self {
            id,
            next_event_idx: AtomicU64::new(next_event_idx),
            policy,
            label,
        }
    }

    pub fn id(&self) -> CursorId {
        self.id
    }

    pub fn next_event_idx(&self) -> u64 {
        self.next_event_idx.load(Ordering::Acquire)
    }

    pub fn policy(&self) -> &CursorPolicy {
        &self.policy
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameHandle {
    pub first_event_idx: u64,
    pub event_count: u32,
    pub generation: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum RingAppendError {
    FrameTooLarge {
        needed: usize,
        capacity: usize,
    },
    ConsumerFellBehind {
        cursor_id: CursorId,
        cursor_label: String,
        next_event_idx: u64,
        required_event_idx: u64,
        lag_us: u64,
    },
}

impl fmt::Display for RingAppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { needed, capacity } => {
                write!(f, "event frame has {needed} events but ring capacity is {capacity}")
            }
            Self::ConsumerFellBehind {
                cursor_label,
                next_event_idx,
                required_event_idx,
                ..
            } => write!(
                f,
                "event consumer {cursor_label:?} fell behind at logical event {next_event_idx}; eviction needs to pass {required_event_idx}"
            ),
        }
    }
}

impl std::error::Error for RingAppendError {}

#[derive(Debug)]
pub struct EventSlice<'a> {
    pub first: &'a [CompactEvent],
    pub second: &'a [CompactEvent],
}

impl EventSlice<'_> {
    pub fn len(&self) -> usize {
        self.first.len() + self.second.len()
    }

    pub fn is_empty(&self) -> bool {
        self.first.is_empty() && self.second.is_empty()
    }
}

#[derive(Debug)]
pub struct EventRing {
    events: Vec<CompactEvent>,
    frame_index: FrameIndex,
    write_head: usize,
    next_event_idx: u64,
    next_cursor_id: u64,
    next_generation: u32,
    cursors: HashMap<CursorId, ConsumerCursor>,
}

impl EventRing {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            events: vec![CompactEvent::default(); capacity],
            frame_index: FrameIndex::default(),
            write_head: 0,
            next_event_idx: 0,
            next_cursor_id: 1,
            next_generation: 1,
            cursors: HashMap::new(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.events.len()
    }

    pub fn frame_index(&self) -> &FrameIndex {
        &self.frame_index
    }

    pub fn retained_event_range(&self) -> Option<Range<u64>> {
        self.frame_index.retained_event_range()
    }

    pub fn resident_event_count(&self) -> u64 {
        self.frame_index
            .entries()
            .map(|entry| u64::from(entry.event_count))
            .sum()
    }

    pub fn resident_byte_count(&self) -> usize {
        usize::try_from(self.resident_event_count())
            .unwrap_or(usize::MAX)
            .saturating_mul(std::mem::size_of::<CompactEvent>())
    }

    pub fn next_event_idx(&self) -> u64 {
        self.next_event_idx
    }

    /// Registers a consumer cursor at the current live head.
    ///
    /// A newly registered cursor observes only frames appended after
    /// registration. This avoids surprising backlog replays for consumers
    /// enabled mid-session; callers that need historical data must fetch it
    /// through an explicit replay/overflow `EventSource`.
    pub fn register_cursor(&mut self, label: impl Into<String>, policy: CursorPolicy) -> CursorId {
        let id = CursorId(self.next_cursor_id);
        self.next_cursor_id = self.next_cursor_id.saturating_add(1);
        let cursor = ConsumerCursor::new(id, self.next_event_idx, policy, label.into());
        self.cursors.insert(id, cursor);
        id
    }

    pub fn unregister_cursor(&mut self, id: CursorId) -> Option<ConsumerCursor> {
        self.cursors.remove(&id)
    }

    pub fn cursor(&self, id: CursorId) -> Option<&ConsumerCursor> {
        self.cursors.get(&id)
    }

    pub fn advance_cursor(&self, id: CursorId, next_event_idx: u64) -> bool {
        let Some(cursor) = self.cursors.get(&id) else {
            return false;
        };
        cursor
            .next_event_idx
            .fetch_max(next_event_idx, Ordering::AcqRel);
        true
    }

    pub fn append_frame(
        &mut self,
        events: &[CompactEvent],
        window_start_us: u64,
        window_end_us: u64,
    ) -> Result<FrameHandle, RingAppendError> {
        let len = events.len();
        let capacity = self.capacity();
        if len > capacity {
            return Err(RingAppendError::FrameTooLarge {
                needed: len,
                capacity,
            });
        }
        if len == 0 {
            return Ok(FrameHandle {
                first_event_idx: self.next_event_idx,
                event_count: 0,
                generation: self.next_generation,
            });
        }

        let physical_start = if self.write_head + len <= capacity {
            self.write_head
        } else {
            0
        };
        let physical_range = physical_start..physical_start + len;
        self.check_eviction_allowed(physical_range.clone())?;

        // Frames are stored oldest-first, but physical positions are not
        // monotonic with age: after a wrap, a younger low-address frame can sit
        // *behind* an older high-address survivor. A wrapped write region can
        // therefore overlap frames that are not a contiguous prefix of the
        // deque. Evicting only while the *front* overlaps would leave such a
        // mid-deque frame in place, letting `copy_from_slice` overwrite its
        // bytes while it still counts toward the resident total (corruption +
        // capacity overflow). We must still evict oldest-first to keep the
        // logical timeline contiguous, so we drop every frame up to and
        // including the newest one that overlaps the write region.
        let evict_through = self
            .frame_index
            .entries()
            .enumerate()
            .filter(|(_, entry)| entry.overlaps_physical(physical_range.clone()))
            .map(|(index, _)| index)
            .max();
        if let Some(last) = evict_through {
            for _ in 0..=last {
                let Some(evicted) = self.frame_index.pop_front() else {
                    break;
                };
                self.advance_best_effort_cursors(evicted.end_event_idx());
            }
        }

        self.events[physical_range.clone()].copy_from_slice(events);
        let first_event_idx = self.next_event_idx;
        let generation = self.next_generation;
        self.next_event_idx = self.next_event_idx.saturating_add(len as u64);
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        self.write_head = physical_range.end;

        self.frame_index.push_back(FrameWindowEntry {
            window_start_us,
            window_end_us,
            first_event_idx,
            event_count: len as u32,
            physical_start,
            generation,
        });

        Ok(FrameHandle {
            first_event_idx,
            event_count: len as u32,
            generation,
        })
    }

    pub fn slice_for_frame(&self, frame_index: usize) -> Option<&[CompactEvent]> {
        let entry = self.frame_index.get(frame_index)?;
        Some(&self.events[entry.physical_range()])
    }

    pub fn event_at(&self, event_idx: u64) -> Option<CompactEvent> {
        let entry = self.frame_index.entry_containing_event(event_idx)?;
        let offset = usize::try_from(event_idx - entry.first_event_idx).ok()?;
        self.events.get(entry.physical_start + offset).copied()
    }

    pub fn collect_event_range(&self, range: Range<u64>, out: &mut Vec<CompactEvent>) -> bool {
        out.clear();
        out.reserve(usize::try_from(range.end - range.start).unwrap_or(usize::MAX));
        if !self.for_each_slice_in_range(range, |slice| out.extend_from_slice(slice)) {
            out.clear();
            return false;
        }
        true
    }

    pub fn for_each_slice_in_range(
        &self,
        range: Range<u64>,
        mut visit: impl FnMut(&[CompactEvent]),
    ) -> bool {
        if range.is_empty() {
            return true;
        }
        let Some(retained) = self.retained_event_range() else {
            return false;
        };
        if range.start < retained.start || range.end > retained.end {
            return false;
        }

        let mut next = range.start;
        for entry in self.frame_index.entries() {
            if entry.end_event_idx() <= range.start {
                continue;
            }
            if entry.first_event_idx >= range.end {
                break;
            }
            if next < entry.first_event_idx {
                return false;
            }

            let segment_start = next.max(entry.first_event_idx);
            let segment_end = range.end.min(entry.end_event_idx());
            let start_offset =
                usize::try_from(segment_start - entry.first_event_idx).unwrap_or(usize::MAX);
            let end_offset =
                usize::try_from(segment_end - entry.first_event_idx).unwrap_or(usize::MAX);
            visit(
                &self.events
                    [entry.physical_start + start_offset..entry.physical_start + end_offset],
            );
            next = segment_end;
            if next == range.end {
                return true;
            }
        }

        false
    }

    pub fn slice_for_range(&self, range: Range<u64>) -> Option<EventSlice<'_>> {
        if range.is_empty() {
            return Some(EventSlice {
                first: &[],
                second: &[],
            });
        }

        let mut first: Option<Range<usize>> = None;
        let mut second: Option<Range<usize>> = None;
        let mut valid = true;

        let complete = self.for_each_slice_in_range(range, |slice| {
            if !valid || slice.is_empty() {
                return;
            }
            let start = unsafe { slice.as_ptr().offset_from(self.events.as_ptr()) } as usize;
            let physical = start..start + slice.len();

            match (&mut first, &mut second) {
                (slot @ None, _) => *slot = Some(physical),
                (Some(first), None) if physical.start == first.end => {
                    first.end = physical.end;
                }
                (Some(first), slot @ None)
                    if first.end == self.capacity() && physical.start == 0 =>
                {
                    *slot = Some(physical);
                }
                (_, Some(second)) if physical.start == second.end => {
                    second.end = physical.end;
                }
                _ => valid = false,
            }
        });

        if !complete || !valid {
            return None;
        }

        let first = first?;
        let second = second.unwrap_or(0..0);
        Some(EventSlice {
            first: &self.events[first],
            second: &self.events[second],
        })
    }

    pub fn frame_entries_from(&self, next_event_idx: u64) -> Option<Vec<FrameWindowEntry>> {
        if next_event_idx > self.next_event_idx {
            return None;
        }
        let Some(retained) = self.retained_event_range() else {
            return Some(Vec::new());
        };
        if next_event_idx < retained.start {
            return None;
        }
        Some(
            self.frame_index
                .entries()
                .filter(|entry| entry.end_event_idx() > next_event_idx)
                .cloned()
                .collect(),
        )
    }

    fn check_eviction_allowed(&self, physical_range: Range<usize>) -> Result<(), RingAppendError> {
        for entry in self
            .frame_index
            .entries()
            .filter(|entry| entry.overlaps_physical(physical_range.clone()))
        {
            for cursor in self.cursors.values() {
                let CursorPolicy::Lossless { backpressure: _ } = cursor.policy() else {
                    continue;
                };
                // TODO(adr-020): dispatch `BackpressureBehavior` here once the
                // writer has a clock/callback surface. Until then the behavior
                // is advisory and callers interpret `ConsumerFellBehind`.
                let next_event_idx = cursor.next_event_idx();
                if next_event_idx < entry.end_event_idx() {
                    return Err(RingAppendError::ConsumerFellBehind {
                        cursor_id: cursor.id(),
                        cursor_label: cursor.label().to_owned(),
                        next_event_idx,
                        required_event_idx: entry.end_event_idx(),
                        lag_us: entry.window_end_us.saturating_sub(entry.window_start_us),
                    });
                }
            }
        }
        Ok(())
    }

    fn advance_best_effort_cursors(&self, evicted_end_event_idx: u64) {
        for cursor in self.cursors.values() {
            if matches!(cursor.policy(), CursorPolicy::BestEffort) {
                cursor
                    .next_event_idx
                    .fetch_max(evicted_end_event_idx, Ordering::AcqRel);
            }
        }
    }
}

impl EventSource for EventRing {
    fn fetch_range(&self, start_us: u64, end_us: u64) -> Result<EventChunk, FetchError> {
        if start_us > end_us {
            return Err(FetchError::OutOfTimeline);
        }

        let mut events = Vec::new();
        for entry in self.frame_index.entries() {
            if entry.window_end_us < start_us || entry.window_start_us > end_us {
                continue;
            }
            let frame = &self.events[entry.physical_range()];
            events.extend_from_slice(inclusive_window(frame, start_us, end_us));
        }

        // Note: an empty result can mean either "no events in the queried
        // range" or "the range lies outside the resident timeline". Callers
        // that need to distinguish these cases should check the ring bounds
        // before calling `fetch_range`.
        if events.is_empty() {
            Err(FetchError::OutOfTimeline)
        } else {
            Ok(EventChunk {
                events,
                triggers: Vec::new(),
                start_us,
                end_us,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackpressureBehavior, CompactEvent, CursorPolicy, EventRing, EventSource, RingAppendError,
    };

    fn event(timestamp_us: u64, x: u16) -> CompactEvent {
        CompactEvent::new(x, 0, timestamp_us, 1)
    }

    #[test]
    fn compact_event_layout_is_ffi_stable() {
        assert_eq!(std::mem::size_of::<CompactEvent>(), 16);
        assert_eq!(std::mem::align_of::<CompactEvent>(), 8);
    }

    #[test]
    fn external_trigger_event_layout_is_ffi_stable() {
        use super::ExternalTriggerEvent;
        assert_eq!(std::mem::size_of::<ExternalTriggerEvent>(), 16);
        assert_eq!(std::mem::align_of::<ExternalTriggerEvent>(), 8);

        let rising = ExternalTriggerEvent::new(42, 3, true);
        assert!(rising.is_rising());
        assert_eq!(rising.id, 3);
        assert_eq!(rising.timestamp_us, 42);
        assert!(!ExternalTriggerEvent::new(42, 0, false).is_rising());
    }

    #[test]
    fn inclusive_trigger_window_selects_inclusive_bounds() {
        use super::{inclusive_trigger_window, ExternalTriggerEvent};
        let triggers = [
            ExternalTriggerEvent::new(10, 0, true),
            ExternalTriggerEvent::new(20, 0, false),
            ExternalTriggerEvent::new(30, 0, true),
        ];
        assert_eq!(inclusive_trigger_window(&triggers, 10, 20).len(), 2);
        assert_eq!(inclusive_trigger_window(&triggers, 11, 19).len(), 0);
        assert_eq!(inclusive_trigger_window(&triggers, 0, 100).len(), 3);
    }

    #[test]
    fn compact_event_new_saturates_unsigned_timestamp_overflow() {
        assert_eq!(
            CompactEvent::new(1, 2, u64::MAX, 1).timestamp_us(),
            i64::MAX as u64
        );
    }

    #[test]
    fn straddling_frame_skips_tail_and_stays_contiguous() {
        let mut ring = EventRing::with_capacity(8);
        ring.append_frame(
            &[
                event(1, 1),
                event(2, 2),
                event(3, 3),
                event(4, 4),
                event(5, 5),
            ],
            1,
            5,
        )
        .expect("first append");
        ring.append_frame(&[event(6, 6), event(7, 7)], 6, 7)
            .expect("second append");

        let handle = ring
            .append_frame(&[event(8, 8), event(9, 9), event(10, 10)], 8, 10)
            .expect("wrap append");
        let newest = ring.frame_index().get(1).expect("newest frame");

        assert_eq!(handle.first_event_idx, 7);
        assert_eq!(newest.physical_start, 0);
        assert_eq!(
            ring.slice_for_frame(1).expect("wrapped frame"),
            &[event(8, 8), event(9, 9), event(10, 10)]
        );
        assert_eq!(ring.event_at(7), Some(event(8, 8)));
    }

    #[test]
    fn global_indices_ignore_padding_across_wraps() {
        let mut ring = EventRing::with_capacity(7);
        let mut next_timestamp = 0u64;

        for len in [3usize, 2, 4, 1, 3, 2, 4] {
            let events: Vec<_> = (0..len)
                .map(|_| {
                    next_timestamp += 1;
                    event(next_timestamp, next_timestamp as u16)
                })
                .collect();
            let handle = ring
                .append_frame(&events, next_timestamp - len as u64 + 1, next_timestamp)
                .expect("append");
            assert_eq!(
                u64::from(handle.event_count),
                events.len() as u64,
                "padding must not become a logical event"
            );
        }

        let mut previous_end = None;
        for entry in ring.frame_index().entries() {
            if let Some(previous_end) = previous_end {
                assert_eq!(entry.first_event_idx, previous_end);
            }
            previous_end = Some(entry.end_event_idx());
        }
    }

    #[test]
    fn oversized_frame_errors_and_leaves_state_unchanged() {
        let mut ring = EventRing::with_capacity(2);
        ring.append_frame(&[event(1, 1)], 1, 1).expect("seed");

        let before_range = ring.retained_event_range();
        let before_next = ring.next_event_idx();
        let err = ring
            .append_frame(&[event(2, 2), event(3, 3), event(4, 4)], 2, 4)
            .expect_err("oversized frame");

        assert_eq!(
            err,
            RingAppendError::FrameTooLarge {
                needed: 3,
                capacity: 2
            }
        );
        assert_eq!(ring.retained_event_range(), before_range);
        assert_eq!(ring.next_event_idx(), before_next);
        assert_eq!(ring.event_at(0), Some(event(1, 1)));
    }

    #[test]
    fn random_append_evict_mix_never_counts_padding_as_events() {
        let mut ring = EventRing::with_capacity(11);
        let mut seed = 0x5eed_u64;
        let mut appended = 0u64;

        for _ in 0..100 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (seed % 5 + 1) as usize;
            let events: Vec<_> = (0..len)
                .map(|offset| event(appended + offset as u64 + 1, offset as u16))
                .collect();
            ring.append_frame(&events, appended + 1, appended + len as u64)
                .expect("append");
            appended += len as u64;

            let retained_count: u64 = ring
                .frame_index()
                .entries()
                .map(|entry| u64::from(entry.event_count))
                .sum();
            let retained_range = ring.retained_event_range().expect("retained events");
            assert_eq!(retained_count, retained_range.end - retained_range.start);

            // The resident events must never exceed the physical capacity, and
            // no two retained frames may claim overlapping physical slots —
            // either would mean eviction left a frame whose bytes were
            // overwritten by a later append.
            assert!(
                retained_count <= ring.capacity() as u64,
                "resident events {retained_count} exceed capacity {}",
                ring.capacity()
            );
            let frames: Vec<_> = ring.frame_index().entries().collect();
            for (i, a) in frames.iter().enumerate() {
                for b in &frames[i + 1..] {
                    assert!(
                        !a.overlaps_physical(b.physical_range()),
                        "retained frames physically overlap: {a:?} vs {b:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn wrapped_write_evicts_overlap_behind_older_survivor() {
        // Regression for an event-ring overflow that crashed the live preview.
        // Two consecutive wraps leave an older high-address frame at the front
        // of the deque while a younger low-address frame sits behind it. The
        // next wrapped write overlaps the younger frame but not the front, so a
        // front-only eviction left it resident — its bytes overwritten and its
        // events double-counted, tripping `resident_byte_count <= capacity`.
        let mut ring = EventRing::with_capacity(3);
        let mut next = 0u64;
        for len in [2usize, 1, 2, 2] {
            let events: Vec<_> = (0..len)
                .map(|_| {
                    next += 1;
                    event(next, next as u16)
                })
                .collect();
            ring.append_frame(&events, next - len as u64 + 1, next)
                .expect("append within capacity must succeed");

            let resident: u64 = ring
                .frame_index()
                .entries()
                .map(|entry| u64::from(entry.event_count))
                .sum();
            assert!(
                resident <= ring.capacity() as u64,
                "resident events {resident} exceed capacity {}",
                ring.capacity()
            );
        }
        // After the final wrap only the newest frame survives.
        assert_eq!(ring.retained_event_range(), Some(5..7));
    }

    #[test]
    fn lossless_cursor_prevents_silent_eviction_until_advanced() {
        let mut ring = EventRing::with_capacity(4);
        let cursor = ring.register_cursor(
            "plugin:test",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );
        ring.append_frame(&[event(1, 1), event(2, 2)], 1, 2)
            .expect("seed");

        let err = ring
            .append_frame(&[event(3, 3), event(4, 4), event(5, 5)], 3, 5)
            .expect_err("cursor blocks eviction");
        assert!(matches!(err, RingAppendError::ConsumerFellBehind { .. }));

        assert!(ring.advance_cursor(cursor, 2));
        ring.append_frame(&[event(3, 3), event(4, 4), event(5, 5)], 3, 5)
            .expect("advanced cursor permits eviction");
        assert_eq!(ring.retained_event_range(), Some(2..5));
    }

    #[test]
    fn best_effort_cursor_jumps_forward_when_evicted() {
        let mut ring = EventRing::with_capacity(4);
        ring.append_frame(&[event(1, 1), event(2, 2)], 1, 2)
            .expect("seed");
        let cursor = ring.register_cursor("preview", CursorPolicy::BestEffort);

        ring.append_frame(&[event(3, 3), event(4, 4), event(5, 5)], 3, 5)
            .expect("best-effort cursor allows eviction");

        assert_eq!(
            ring.cursor(cursor).expect("cursor").next_event_idx(),
            2,
            "best-effort consumers observe the skipped logical prefix"
        );
    }

    #[test]
    fn collect_event_range_skips_physical_padding_between_wrapped_frames() {
        let mut ring = EventRing::with_capacity(8);
        ring.append_frame(
            &[
                event(1, 1),
                event(2, 2),
                event(3, 3),
                event(4, 4),
                event(5, 5),
            ],
            1,
            5,
        )
        .expect("seed");
        ring.append_frame(&[event(6, 6)], 6, 6).expect("tail frame");
        ring.append_frame(&[event(7, 7), event(8, 8), event(9, 9)], 7, 9)
            .expect("wrapped frame");

        let mut events = Vec::new();
        assert!(ring.collect_event_range(5..9, &mut events));
        assert_eq!(
            events,
            vec![event(6, 6), event(7, 7), event(8, 8), event(9, 9)]
        );
        assert!(
            ring.slice_for_range(5..9).is_none(),
            "a two-slice range cannot include skip-on-straddle padding"
        );
    }

    #[test]
    fn register_cursor_starts_at_current_head_without_backlog() {
        let mut ring = EventRing::with_capacity(8);
        ring.append_frame(&[event(1, 1), event(2, 2)], 1, 2)
            .expect("seed");

        let cursor = ring.register_cursor(
            "plugin:late",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );

        assert_eq!(ring.cursor(cursor).expect("cursor").next_event_idx(), 2);
        assert_eq!(ring.frame_entries_from(2), Some(Vec::new()));
    }

    #[test]
    fn event_source_fetches_by_timestamp() {
        let mut ring = EventRing::with_capacity(6);
        ring.append_frame(&[event(10, 1), event(20, 2), event(30, 3)], 10, 30)
            .expect("append");

        let chunk = ring.fetch_range(15, 25).expect("fetch");

        assert_eq!(chunk.events, vec![event(20, 2)]);
    }
}
