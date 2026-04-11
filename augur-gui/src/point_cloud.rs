use std::collections::VecDeque;

use augur_core::pipeline::CdEvent;

const DEFAULT_TIME_WINDOW_MS: f32 = 120.0;
const MIN_TIME_WINDOW_MS: f32 = 5.0;
const MAX_TIME_WINDOW_MS: f32 = MAX_HISTORY_MS;
const DEFAULT_POINT_LIMIT: usize = 12_000;
const MIN_POINT_LIMIT: usize = 1_000;
const MAX_POINT_LIMIT: usize = 100_000;
const MAX_HISTORY_POINTS: usize = 400_000;
const MAX_HISTORY_MS: f32 = 5_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleHistoryWindow {
    lo: usize,
    hi: usize,
    step: usize,
}

#[derive(Debug, Clone)]
pub struct PointCloudState {
    history: VecDeque<CdEvent>,
    pub time_window_ms: f32,
    pub point_limit: usize,
}

impl Default for PointCloudState {
    fn default() -> Self {
        Self {
            history: VecDeque::with_capacity(32_768),
            time_window_ms: DEFAULT_TIME_WINDOW_MS,
            point_limit: DEFAULT_POINT_LIMIT,
        }
    }
}

impl PointCloudState {
    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn push_events(&mut self, events: &[CdEvent]) {
        if events.is_empty() {
            return;
        }

        let events = if events.len() >= MAX_HISTORY_POINTS {
            self.history.clear();
            &events[events.len() - MAX_HISTORY_POINTS..]
        } else {
            events
        };

        self.history.extend(events.iter().copied());
        self.trim_history();
    }

    pub fn sanitize_controls(&mut self) {
        self.time_window_ms = self
            .time_window_ms
            .clamp(MIN_TIME_WINDOW_MS, MAX_TIME_WINDOW_MS);
        self.point_limit = self.point_limit.clamp(MIN_POINT_LIMIT, MAX_POINT_LIMIT);
    }

    pub fn visible_events(&self) -> Vec<CdEvent> {
        let Some(window) = self.visible_history_window() else {
            return Vec::new();
        };
        self.history
            .range(window.lo..window.hi)
            .step_by(window.step)
            .copied()
            .collect()
    }

    pub fn visible_event_count(&self) -> usize {
        self.visible_history_window()
            .map(|window| (window.hi - window.lo).div_ceil(window.step))
            .unwrap_or(0)
    }

    pub fn visible_time_span_ms(&self) -> Option<f32> {
        let window = self.visible_history_window()?;
        let first = self.history.get(window.lo)?;
        let last = self.history.get(window.hi.saturating_sub(1))?;
        Some(last.timestamp.saturating_sub(first.timestamp) as f32 / 1_000.0)
    }

    pub fn effective_time_window_ms(&self) -> f32 {
        self.visible_time_span_ms()
            .unwrap_or(self.time_window_ms)
            .max(1.0)
    }

    fn trim_history(&mut self) {
        let len = self.history.len();
        if len > MAX_HISTORY_POINTS {
            self.history.drain(..len - MAX_HISTORY_POINTS);
        }

        let Some(latest) = self.history.back() else {
            return;
        };
        let cutoff = latest
            .timestamp
            .saturating_sub((MAX_HISTORY_MS * 1_000.0).round() as u64);
        let cutoff_idx = self
            .history
            .partition_point(|event| event.timestamp < cutoff);
        if cutoff_idx > 0 {
            self.history.drain(..cutoff_idx);
        }
    }

    fn visible_history_window(&self) -> Option<VisibleHistoryWindow> {
        let latest = self.history.back()?;
        let end_ts = latest.timestamp;
        let start_ts = end_ts.saturating_sub((self.time_window_ms * 1_000.0).round() as u64);
        let lo = self
            .history
            .partition_point(|event| event.timestamp < start_ts);
        let hi = self
            .history
            .partition_point(|event| event.timestamp <= end_ts);
        if lo >= hi {
            return None;
        }

        let step = (hi - lo).div_ceil(self.point_limit.max(1)).max(1);
        Some(VisibleHistoryWindow { lo, hi, step })
    }
}

#[cfg(test)]
mod tests {
    use super::PointCloudState;
    use augur_core::pipeline::CdEvent;

    fn event(timestamp: u64, x: u16, y: u16) -> CdEvent {
        CdEvent {
            timestamp,
            x,
            y,
            polarity: true,
        }
    }

    #[test]
    fn sanitize_controls_clamps_values() {
        let mut state = PointCloudState {
            time_window_ms: 0.0,
            point_limit: 1,
            ..PointCloudState::default()
        };

        state.sanitize_controls();

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
        state.push_events(&[
            event(0, 0, 0),
            event(500, 1, 0),
            event(1_000, 2, 0),
            event(1_250, 3, 0),
            event(1_500, 4, 0),
        ]);

        let visible = state.visible_events();

        assert_eq!(visible.len(), 2);
        assert!(visible.iter().all(|event| event.timestamp >= 500));
    }

    #[test]
    fn clear_drops_history() {
        let mut state = PointCloudState::default();
        state.push_events(&[event(10, 1, 1), event(20, 2, 2)]);

        state.clear();

        assert!(state.visible_events().is_empty());
        assert_eq!(state.visible_event_count(), 0);
    }

    #[test]
    fn effective_time_window_tracks_retained_history_span() {
        let mut state = PointCloudState {
            time_window_ms: 400.0,
            point_limit: 8,
            ..PointCloudState::default()
        };
        state.push_events(&[
            event(10_000, 0, 0),
            event(10_500, 1, 0),
            event(11_000, 2, 0),
        ]);

        assert_eq!(state.visible_time_span_ms(), Some(1.0));
        assert_eq!(state.effective_time_window_ms(), 1.0);
    }
}
