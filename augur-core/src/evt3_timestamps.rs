pub(crate) const EVT3_TIMESTAMP_PERIOD_US: u64 = 1 << 24;
const EVT3_WRAP_THRESHOLD_US: u64 = EVT3_TIMESTAMP_PERIOD_US / 2;

#[derive(Debug, Clone, Default)]
pub(crate) struct Evt3TimestampUnwrapper {
    wrap_count: u64,
    last_raw_timestamp_us: Option<u64>,
    last_timestamp_us: Option<u64>,
    expected_timestamp_us: Option<u64>,
}

impl Evt3TimestampUnwrapper {
    pub(crate) fn with_expected_timestamp(expected_timestamp_us: u64) -> Self {
        Self {
            expected_timestamp_us: Some(expected_timestamp_us),
            ..Self::default()
        }
    }

    pub(crate) fn map_timestamp(&mut self, raw_timestamp_us: u64) -> u64 {
        let mut timestamp_us = if let Some(last_raw_timestamp_us) = self.last_raw_timestamp_us {
            if raw_timestamp_us < last_raw_timestamp_us
                && last_raw_timestamp_us - raw_timestamp_us > EVT3_WRAP_THRESHOLD_US
            {
                self.wrap_count = self.wrap_count.saturating_add(1);
            }
            raw_timestamp_us
                .saturating_add(self.wrap_count.saturating_mul(EVT3_TIMESTAMP_PERIOD_US))
        } else {
            let timestamp_us = self
                .expected_timestamp_us
                .take()
                .map(|expected_timestamp_us| {
                    nearest_evt3_timestamp_candidate(raw_timestamp_us, expected_timestamp_us)
                })
                .unwrap_or(raw_timestamp_us);
            self.wrap_count = timestamp_us / EVT3_TIMESTAMP_PERIOD_US;
            timestamp_us
        };

        if let Some(last_timestamp_us) = self.last_timestamp_us {
            timestamp_us = timestamp_us.max(last_timestamp_us);
        }

        self.last_raw_timestamp_us = Some(raw_timestamp_us);
        self.last_timestamp_us = Some(timestamp_us);
        timestamp_us
    }
}

fn nearest_evt3_timestamp_candidate(raw_timestamp_us: u64, expected_timestamp_us: u64) -> u64 {
    let period = EVT3_TIMESTAMP_PERIOD_US;
    let wrap_guess = expected_timestamp_us / period;
    [
        wrap_guess.saturating_sub(1),
        wrap_guess,
        wrap_guess.saturating_add(1),
    ]
    .into_iter()
    .map(|wrap_count| raw_timestamp_us.saturating_add(wrap_count.saturating_mul(period)))
    .min_by_key(|&candidate| candidate.abs_diff(expected_timestamp_us))
    .unwrap_or(raw_timestamp_us)
}

#[cfg(test)]
mod tests {
    use super::{Evt3TimestampUnwrapper, EVT3_TIMESTAMP_PERIOD_US};

    #[test]
    fn maps_initial_timestamp_to_nearest_expected_rollover_epoch() {
        let expected_timestamp_us = EVT3_TIMESTAMP_PERIOD_US + 64;
        let mut unwrapper = Evt3TimestampUnwrapper::with_expected_timestamp(expected_timestamp_us);

        assert_eq!(unwrapper.map_timestamp(32), EVT3_TIMESTAMP_PERIOD_US + 32);
    }

    #[test]
    fn unwraps_rollover_monotonically() {
        let mut unwrapper = Evt3TimestampUnwrapper::default();
        let before_wrap = EVT3_TIMESTAMP_PERIOD_US - 8;

        assert_eq!(unwrapper.map_timestamp(before_wrap), before_wrap);
        assert_eq!(unwrapper.map_timestamp(12), EVT3_TIMESTAMP_PERIOD_US + 12);
    }
}
