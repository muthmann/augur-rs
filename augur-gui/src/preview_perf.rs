use std::time::Duration;

#[derive(Debug, Clone, Copy, Default)]
pub struct PerfMetricSnapshot {
    pub last_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub samples: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PreviewPerfSnapshot {
    pub frame_total: PerfMetricSnapshot,
    pub dequeue: PerfMetricSnapshot,
    pub analysis: PerfMetricSnapshot,
    pub histogram: PerfMetricSnapshot,
    pub line_profile: PerfMetricSnapshot,
    pub cpu_fallback_render: PerfMetricSnapshot,
    pub upload_submit: PerfMetricSnapshot,
    pub external_bridge: PerfMetricSnapshot,
}

#[derive(Debug, Clone, Copy, Default)]
struct PerfMetric {
    last_ms: f64,
    avg_ms: f64,
    max_ms: f64,
    samples: u64,
}

impl PerfMetric {
    fn record(&mut self, duration: Duration) {
        let millis = duration.as_secs_f64() * 1_000.0;
        self.last_ms = millis;
        self.samples = self.samples.saturating_add(1);
        if self.samples == 1 {
            self.avg_ms = millis;
        } else {
            let samples = self.samples as f64;
            self.avg_ms += (millis - self.avg_ms) / samples;
        }
        self.max_ms = self.max_ms.max(millis);
    }

    fn snapshot(self) -> PerfMetricSnapshot {
        PerfMetricSnapshot {
            last_ms: self.last_ms,
            avg_ms: self.avg_ms,
            max_ms: self.max_ms,
            samples: self.samples,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct PreviewPerfStats {
    frame_total: PerfMetric,
    dequeue: PerfMetric,
    analysis: PerfMetric,
    histogram: PerfMetric,
    line_profile: PerfMetric,
    cpu_fallback_render: PerfMetric,
    upload_submit: PerfMetric,
    external_bridge: PerfMetric,
}

impl PreviewPerfStats {
    pub fn record_frame_total(&mut self, duration: Duration) {
        self.frame_total.record(duration);
    }

    pub fn record_dequeue(&mut self, duration: Duration) {
        self.dequeue.record(duration);
    }

    pub fn record_analysis(&mut self, duration: Duration) {
        self.analysis.record(duration);
    }

    pub fn record_histogram(&mut self, duration: Duration) {
        self.histogram.record(duration);
    }

    pub fn record_line_profile(&mut self, duration: Duration) {
        self.line_profile.record(duration);
    }

    pub fn record_cpu_fallback_render(&mut self, duration: Duration) {
        self.cpu_fallback_render.record(duration);
    }

    pub fn record_upload_submit(&mut self, duration: Duration) {
        self.upload_submit.record(duration);
    }

    pub fn record_external_bridge(&mut self, duration: Duration) {
        self.external_bridge.record(duration);
    }

    pub fn snapshot(&self) -> PreviewPerfSnapshot {
        PreviewPerfSnapshot {
            frame_total: self.frame_total.snapshot(),
            dequeue: self.dequeue.snapshot(),
            analysis: self.analysis.snapshot(),
            histogram: self.histogram.snapshot(),
            line_profile: self.line_profile.snapshot(),
            cpu_fallback_render: self.cpu_fallback_render.snapshot(),
            upload_submit: self.upload_submit.snapshot(),
            external_bridge: self.external_bridge.snapshot(),
        }
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
