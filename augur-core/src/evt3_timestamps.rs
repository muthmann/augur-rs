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

    /// The unwrapped timestamp this stream currently sits at, or the caller's
    /// expected start hint before the first mapped timestamp. Used as the
    /// rollover-epoch reference for interleaved secondary streams (triggers).
    pub(crate) fn reference_timestamp_us(&self) -> Option<u64> {
        self.last_timestamp_us.or(self.expected_timestamp_us)
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

/// Unwraps EVT3 trigger timestamps against the rollover epoch of the CD
/// stream decoded from the same packets.
///
/// Triggers arrive in a separate vector from CD events, so feeding them
/// through the CD unwrapper sequentially would either count spurious wraps
/// or clamp mid-packet trigger times up to the packet's last CD timestamp.
/// Triggers are also too sparse to self-unwrap (two edges more than half a
/// period apart defeat the wrap heuristic). Instead each trigger picks the
/// rollover candidate nearest to a reference taken from the CD unwrapper
/// (or the last mapped trigger, whichever is later), which is exact while
/// packets are shorter than half the 2^24 µs period — always true in
/// practice. Monotonicity is enforced within the trigger stream only.
#[derive(Debug, Clone, Default)]
pub(crate) struct SecondaryTimestampMapper {
    last_timestamp_us: Option<u64>,
}

impl SecondaryTimestampMapper {
    pub(crate) fn map_timestamp(
        &mut self,
        raw_timestamp_us: u64,
        reference_us: Option<u64>,
    ) -> u64 {
        let reference_us = match (reference_us, self.last_timestamp_us) {
            (Some(reference), Some(last)) => reference.max(last),
            (Some(reference), None) => reference,
            (None, Some(last)) => last,
            (None, None) => raw_timestamp_us,
        };
        let mut timestamp_us = nearest_evt3_timestamp_candidate(raw_timestamp_us, reference_us);
        if let Some(last) = self.last_timestamp_us {
            timestamp_us = timestamp_us.max(last);
        }
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

    #[test]
    fn secondary_mapper_follows_cd_epoch_across_rollover() {
        use super::SecondaryTimestampMapper;
        let mut cd = Evt3TimestampUnwrapper::default();
        let mut triggers = SecondaryTimestampMapper::default();

        // CD stream crosses the rollover; the trigger raw timestamps sit on
        // either side of the wrap within the same packet.
        let before_wrap = EVT3_TIMESTAMP_PERIOD_US - 8;
        cd.map_timestamp(before_wrap);
        assert_eq!(
            triggers.map_timestamp(before_wrap - 2, cd.reference_timestamp_us()),
            before_wrap - 2
        );

        cd.map_timestamp(12); // CD unwraps to period + 12
        assert_eq!(
            triggers.map_timestamp(4, cd.reference_timestamp_us()),
            EVT3_TIMESTAMP_PERIOD_US + 4
        );
    }

    #[test]
    fn secondary_mapper_does_not_clamp_mid_packet_triggers_to_cd_tail() {
        use super::SecondaryTimestampMapper;
        let mut cd = Evt3TimestampUnwrapper::default();
        let mut triggers = SecondaryTimestampMapper::default();

        // A packet's CD events run to t=5_000, while a trigger fired at
        // t=1_000 inside the same packet: the trigger must keep its own time.
        cd.map_timestamp(5_000);
        assert_eq!(
            triggers.map_timestamp(1_000, cd.reference_timestamp_us()),
            1_000
        );
    }

    #[test]
    fn secondary_mapper_stays_monotonic_without_cd_reference() {
        use super::SecondaryTimestampMapper;
        let mut triggers = SecondaryTimestampMapper::default();
        assert_eq!(triggers.map_timestamp(100, None), 100);
        assert_eq!(triggers.map_timestamp(50, None), 100);
    }
}
