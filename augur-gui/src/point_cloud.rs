use std::{cell::RefCell, collections::VecDeque, ops::Range, sync::Arc};

use augur_core::pipeline::{CdEvent, LiveEventSource, PreviewFrame};
use augur_event_types::FrameWindowEntry;

const DEFAULT_TIME_WINDOW_MS: f32 = 120.0;
const MIN_TIME_WINDOW_MS: f32 = 5.0;
const MAX_TIME_WINDOW_MS: f32 = MAX_HISTORY_MS;
const DEFAULT_POINT_LIMIT: usize = 12_000;
const MIN_POINT_LIMIT: usize = 1_000;
const MAX_POINT_LIMIT: usize = 100_000;
const MAX_HISTORY_POINTS: usize = 400_000;
const MAX_HISTORY_MS: f32 = 5_000.0;

#[derive(Debug, Clone)]
struct RetainedEventFrame {
    source: LiveEventSource,
    event_range: Range<u64>,
    window_start_us: u64,
    window_end_us: u64,
    event_count: usize,
}

#[derive(Debug, Clone)]
pub struct PointCloudState {
    frames: VecDeque<RetainedEventFrame>,
    pub time_window_ms: f32,
    pub point_limit: usize,
    /// Bumped whenever `frames` changes so a stale memoized summary is rejected.
    generation: u64,
    /// Memoized result of the last `visible_summary_at` so the per-frame scene
    /// build and footer status don't each re-scan and re-decode the ring.
    summary_cache: RefCell<Option<CachedSummary>>,
}

#[derive(Debug, Clone, PartialEq)]
struct SummaryKey {
    anchor_end_us: u64,
    time_window_bits: u32,
    point_limit: usize,
    generation: u64,
}

#[derive(Debug, Clone)]
struct CachedSummary {
    key: SummaryKey,
    value: VisiblePointCloudEvents,
}

#[derive(Debug, Clone, Default)]
pub struct VisiblePointCloudEvents {
    /// Shared so the memoized summary can be handed out per UI frame without
    /// cloning up to `point_limit` events each time.
    pub events: Arc<[CdEvent]>,
    pub retained_time_span_ms: Option<f32>,
    pub sampled_count: usize,
    pub effective_time_window_ms: f32,
}

impl Default for PointCloudState {
    fn default() -> Self {
        Self {
            frames: VecDeque::with_capacity(512),
            time_window_ms: DEFAULT_TIME_WINDOW_MS,
            point_limit: DEFAULT_POINT_LIMIT,
            generation: 0,
            summary_cache: RefCell::new(None),
        }
    }
}

impl PointCloudState {
    pub fn clear(&mut self) {
        self.frames.clear();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn push_frame(&mut self, frame: &PreviewFrame) {
        let Some(source) = frame.event_source.clone() else {
            return;
        };
        let Some(event_range) = frame.event_range.clone() else {
            return;
        };
        let Some(event_count) = frame.event_count() else {
            return;
        };
        if event_count == 0 {
            return;
        }

        if event_count >= MAX_HISTORY_POINTS {
            self.frames.clear();
        } else {
            self.trim_to_point_budget(MAX_HISTORY_POINTS.saturating_sub(event_count));
        }

        self.frames.push_back(RetainedEventFrame {
            source,
            event_range,
            window_start_us: frame.window_start_us,
            window_end_us: frame.window_end_us,
            event_count,
        });
        self.trim_history();
        self.generation = self.generation.wrapping_add(1);
    }

    /// Whether the retained history fully spans `[start_us, end_us]`, so an
    /// anchored summary inside that window can be served without refilling.
    pub fn covers_window(&self, start_us: u64, end_us: u64) -> bool {
        let (Some(front), Some(back)) = (self.frames.front(), self.frames.back()) else {
            return false;
        };
        front.window_start_us <= start_us && back.window_end_us >= end_us
    }

    /// Replaces the retained history with the frame windows currently
    /// resident in the upstream ring. Used after a replay seek: the seek
    /// sprint archives every decoded frame into the ring even when the
    /// bounded preview channel drops the frame object, so rebuilding from the
    /// ring gives the 3D view a gap-free look-back window.
    pub fn rebuild_from_source_frames(
        &mut self,
        source: &LiveEventSource,
        entries: &[FrameWindowEntry],
    ) {
        self.frames.clear();
        for entry in entries {
            let event_count = entry.event_count as usize;
            if event_count == 0 {
                continue;
            }
            if event_count >= MAX_HISTORY_POINTS {
                self.frames.clear();
            } else {
                self.trim_to_point_budget(MAX_HISTORY_POINTS.saturating_sub(event_count));
            }
            self.frames.push_back(RetainedEventFrame {
                source: source.clone(),
                event_range: entry.first_event_idx..entry.end_event_idx(),
                window_start_us: entry.window_start_us,
                window_end_us: entry.window_end_us,
                event_count,
            });
        }
        self.trim_history();
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn sanitize_controls(&mut self) {
        self.time_window_ms = self
            .time_window_ms
            .clamp(MIN_TIME_WINDOW_MS, MAX_TIME_WINDOW_MS);
        self.point_limit = self.point_limit.clamp(MIN_POINT_LIMIT, MAX_POINT_LIMIT);
    }

    pub fn visible_summary(&self) -> VisiblePointCloudEvents {
        let Some(latest) = self.frames.back() else {
            return self.empty_visible_summary();
        };
        self.visible_summary_at(latest.window_end_us)
    }

    pub fn visible_summary_at(&self, anchor_end_us: u64) -> VisiblePointCloudEvents {
        let key = SummaryKey {
            anchor_end_us,
            time_window_bits: self.time_window_ms.to_bits(),
            point_limit: self.point_limit,
            generation: self.generation,
        };
        if let Some(cached) = self.summary_cache.borrow().as_ref() {
            if cached.key == key {
                return cached.value.clone();
            }
        }
        let value = self.compute_visible_summary_at(anchor_end_us);
        *self.summary_cache.borrow_mut() = Some(CachedSummary {
            key,
            value: value.clone(),
        });
        value
    }

    fn compute_visible_summary_at(&self, anchor_end_us: u64) -> VisiblePointCloudEvents {
        let mut events = self.visible_event_candidates_at(anchor_end_us);
        if events.is_empty() {
            return self.empty_visible_summary();
        }

        let retained_time_span_ms = events
            .first()
            .zip(events.last())
            .map(|(first, last)| last.timestamp.saturating_sub(first.timestamp) as f32 / 1_000.0);
        let step = events.len().div_ceil(self.point_limit.max(1)).max(1);
        if step > 1 {
            let mut sampled = Vec::with_capacity(events.len() / step + 1);
            sampled.extend(events.into_iter().step_by(step));
            events = sampled;
        }
        let sampled_count = events.len();
        VisiblePointCloudEvents {
            events: events.into(),
            retained_time_span_ms,
            sampled_count,
            effective_time_window_ms: retained_time_span_ms
                .unwrap_or(self.time_window_ms)
                .max(1.0),
        }
    }

    fn empty_visible_summary(&self) -> VisiblePointCloudEvents {
        VisiblePointCloudEvents {
            events: Arc::from(Vec::new()),
            retained_time_span_ms: None,
            sampled_count: 0,
            effective_time_window_ms: self.time_window_ms.max(1.0),
        }
    }

    fn trim_history(&mut self) {
        self.trim_to_point_budget(MAX_HISTORY_POINTS);

        let Some(latest) = self.frames.back() else {
            return;
        };
        let cutoff = latest
            .window_end_us
            .saturating_sub((MAX_HISTORY_MS * 1_000.0).round() as u64);
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.window_end_us < cutoff)
        {
            self.frames.pop_front();
        }
    }

    fn trim_to_point_budget(&mut self, max_points: usize) {
        let mut total = self.retained_event_count();
        while self.frames.len() > 1 && total > max_points {
            total -= self.frames.pop_front().map(|f| f.event_count).unwrap_or(0);
        }
    }

    fn retained_event_count(&self) -> usize {
        self.frames.iter().map(|frame| frame.event_count).sum()
    }

    fn visible_event_candidates_at(&self, end_ts: u64) -> Vec<CdEvent> {
        let start_ts = end_ts.saturating_sub((self.time_window_ms * 1_000.0).round() as u64);
        let overlapping = |frame: &&RetainedEventFrame| {
            frame.window_end_us >= start_ts && frame.window_start_us <= end_ts
        };
        let mut events = Vec::with_capacity(
            self.frames
                .iter()
                .filter(overlapping)
                .map(|frame| frame.event_count)
                .sum(),
        );
        for frame in self.frames.iter().filter(overlapping) {
            // Zero-copy visit straight out of the shared ring: the previous
            // per-frame `events_for_range` path allocated two vectors per
            // retained frame on every recompute.
            frame
                .source
                .for_each_compact_event_in_range(frame.event_range.clone(), |compact| {
                    let event = CdEvent::from(compact);
                    if event.timestamp >= start_ts && event.timestamp <= end_ts {
                        events.push(event);
                    }
                });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::PointCloudState;
    use augur_core::pipeline::{CdEvent, LiveEventSource, PreviewFrame};

    fn event(timestamp: u64, x: u16, y: u16) -> CdEvent {
        CdEvent {
            timestamp,
            x,
            y,
            polarity: true,
        }
    }

    fn frame(events: &[CdEvent]) -> PreviewFrame {
        let source = LiveEventSource::default();
        let window_start_us = events.first().map_or(0, |event| event.timestamp);
        let window_end_us = events
            .last()
            .map_or(window_start_us, |event| event.timestamp);
        let event_range = source
            .append_cd_frame(events, window_start_us, window_end_us)
            .expect("frame events must append");
        PreviewFrame {
            width: 1280,
            height: 720,
            pixels: Vec::new(),
            pixels_on: Vec::new(),
            pixels_off: Vec::new(),
            cached_total_histogram: Vec::new(),
            cached_signed_histogram: Vec::new(),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: Some(event_range),
            event_source: Some(source),
            window_start_us,
            window_end_us,
        }
    }

    #[test]
    fn default_controls_have_sane_values() {
        let state = PointCloudState::default();
        assert!(state.time_window_ms >= 5.0);
        assert!(state.point_limit >= 1_000);
    }

    #[test]
    fn visible_events_respect_time_window_and_limit() {
        let mut state = PointCloudState {
            time_window_ms: 1.0,
            point_limit: 2,
            ..PointCloudState::default()
        };
        state.push_frame(&frame(&[
            event(0, 0, 0),
            event(500, 1, 0),
            event(1_000, 2, 0),
            event(1_250, 3, 0),
            event(1_500, 4, 0),
        ]));

        let visible = state.visible_summary().events;

        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|event| event.timestamp >= 500));
    }

    #[test]
    fn clear_drops_history() {
        let mut state = PointCloudState::default();
        state.push_frame(&frame(&[event(10, 1, 1), event(20, 2, 2)]));

        state.clear();

        let summary = state.visible_summary();
        assert!(summary.events.is_empty());
        assert_eq!(summary.sampled_count, 0);
    }

    #[test]
    fn effective_time_window_tracks_retained_history_span() {
        let mut state = PointCloudState {
            time_window_ms: 400.0,
            point_limit: 8,
            ..PointCloudState::default()
        };
        state.push_frame(&frame(&[
            event(10_000, 0, 0),
            event(10_500, 1, 0),
            event(11_000, 2, 0),
        ]));

        let summary = state.visible_summary();
        assert_eq!(summary.retained_time_span_ms, Some(1.0));
        assert_eq!(summary.effective_time_window_ms, 1.0);
    }

    #[test]
    fn covers_window_reflects_retained_span() {
        let mut state = PointCloudState::default();
        assert!(!state.covers_window(0, 10));

        state.push_frame(&frame(&[event(1_000, 0, 0), event(2_000, 1, 0)]));
        state.push_frame(&frame(&[event(3_000, 2, 0), event(4_000, 3, 0)]));

        assert!(state.covers_window(1_000, 4_000));
        assert!(state.covers_window(2_000, 3_000));
        assert!(!state.covers_window(500, 3_000), "starts before history");
        assert!(!state.covers_window(2_000, 5_000), "ends after history");
    }

    #[test]
    fn rebuild_from_source_frames_replaces_history_from_ring_entries() {
        use augur_core::pipeline::LiveEventSource;

        let mut state = PointCloudState {
            time_window_ms: 100.0,
            ..PointCloudState::default()
        };
        // Stale history that must be replaced.
        state.push_frame(&frame(&[event(99_000, 9, 9)]));

        let source = LiveEventSource::with_capacity(16);
        source
            .append_cd_frame(&[event(1_000, 0, 0), event(2_000, 1, 0)], 1_000, 2_000)
            .expect("first ring frame appends");
        source
            .append_cd_frame(&[event(3_000, 2, 0)], 2_000, 3_000)
            .expect("second ring frame appends");
        let entries = source.retained_frame_entries();
        assert_eq!(entries.len(), 2);

        state.rebuild_from_source_frames(&source, &entries);

        let summary = state.visible_summary_at(3_000);
        assert_eq!(
            summary
                .events
                .iter()
                .map(|event| event.timestamp)
                .collect::<Vec<_>>(),
            vec![1_000, 2_000, 3_000],
            "rebuilt history must reflect exactly the ring frames",
        );
        assert!(state.covers_window(1_000, 3_000));
        assert!(!state.covers_window(99_000, 99_000));
    }

    #[test]
    fn visible_summary_can_anchor_to_displayed_frame_end() {
        let mut state = PointCloudState {
            time_window_ms: 2.0,
            point_limit: 16,
            ..PointCloudState::default()
        };
        state.push_frame(&frame(&[event(0, 0, 0), event(1_000, 1, 0)]));
        state.push_frame(&frame(&[event(2_000, 2, 0), event(3_000, 3, 0)]));

        let anchored = state.visible_summary_at(1_000);
        let latest = state.visible_summary();

        assert_eq!(
            anchored
                .events
                .iter()
                .map(|event| event.timestamp)
                .collect::<Vec<_>>(),
            vec![0, 1_000]
        );
        assert_eq!(
            latest
                .events
                .iter()
                .map(|event| event.timestamp)
                .collect::<Vec<_>>(),
            vec![1_000, 2_000, 3_000]
        );
    }
}
