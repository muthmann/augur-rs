use std::{collections::VecDeque, ops::Range};

use augur_core::pipeline::{CdEvent, LiveEventSource, PreviewFrame};

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
}

#[derive(Debug, Clone, Default)]
pub struct VisiblePointCloudEvents {
    pub events: Vec<CdEvent>,
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
        }
    }
}

impl PointCloudState {
    pub fn clear(&mut self) {
        self.frames.clear();
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
            events,
            retained_time_span_ms,
            sampled_count,
            effective_time_window_ms: retained_time_span_ms
                .unwrap_or(self.time_window_ms)
                .max(1.0),
        }
    }

    fn empty_visible_summary(&self) -> VisiblePointCloudEvents {
        VisiblePointCloudEvents {
            events: Vec::new(),
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
        let mut events = Vec::new();
        for frame in &self.frames {
            if frame.window_end_us < start_ts || frame.window_start_us > end_ts {
                continue;
            }
            let Some(mut frame_events) = frame.source.events_for_range(frame.event_range.clone())
            else {
                continue;
            };
            frame_events.retain(|event| event.timestamp >= start_ts && event.timestamp <= end_ts);
            events.extend(frame_events);
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
