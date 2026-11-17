//! Frame timing estimates from presentation feedback for Linux redraw
//! scheduling.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

const LAG_WINDOW: usize = 64;

pub struct FrameClock {
    last: Option<Instant>,
    period: Duration,
    cost: Duration,
    margin: Duration,
    targets: BTreeMap<usize, Instant>,
    lags: VecDeque<u128>,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self {
            last: None,
            period: Duration::ZERO,
            cost: Duration::ZERO,
            margin: Duration::from_millis(4),
            targets: BTreeMap::new(),
            lags: VecDeque::new(),
        }
    }
}

impl FrameClock {
    #[cfg(target_os = "linux")]
    pub fn timings(&self) -> (Duration, Duration, Duration) {
        (self.cost, self.margin, self.period)
    }

    pub fn presented(&mut self, seq: usize, time: Instant, refresh: Duration) {
        if !refresh.is_zero() {
            if refresh != self.period {
                self.lags.clear();
            }
            self.period = refresh;
        }
        if let Some(target) = self.targets.remove(&seq)
            && !self.period.is_zero()
        {
            let period = self.period.as_nanos();
            let lag = (time.saturating_duration_since(target).as_nanos() + period / 2) / period;
            self.lags.push_back(lag);
            if self.lags.len() > LAG_WINDOW {
                self.lags.pop_front();
            }
            let baseline = *self.lags.iter().min().unwrap();
            self.margin = if lag > baseline {
                (self.margin + Duration::from_millis(1)).min(self.period)
            } else {
                self.margin
                    .saturating_sub(Duration::from_micros(100))
                    .max(Duration::from_millis(1))
                    .min(self.period)
            };
        }
        self.last = Some(self.last.map_or(time, |last| last.max(time)));
    }

    pub fn discarded(&mut self, seq: usize) {
        self.targets.remove(&seq);
    }

    pub fn submitted(&mut self, seq: usize, target: Option<Instant>, cost: Duration) {
        self.cost = cost.max(self.cost.mul_f64(0.9));
        if let Some(target) = target {
            self.targets.insert(seq, target);
        }
    }

    pub fn deadline(&self, now: Instant) -> Option<(Instant, Instant)> {
        let last = self.last?;
        if self.period.is_zero() {
            return None;
        }
        let earliest = now.checked_add(self.cost + self.margin)?;
        let elapsed = earliest.saturating_duration_since(last).as_nanos();
        let periods = elapsed.div_ceil(self.period.as_nanos()).max(1);
        let offset = self.period.checked_mul(u32::try_from(periods).ok()?)?;
        let target = last.checked_add(offset)?;
        Some((target.checked_sub(self.cost + self.margin)?, target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_refresh_falls_back_and_zero_keeps_known_period() {
        let now = Instant::now();
        let mut clock = FrameClock::default();
        assert_eq!(clock.deadline(now), None);
        clock.presented(0, now, Duration::ZERO);
        assert_eq!(clock.deadline(now), None);
        clock.presented(1, now, Duration::from_millis(16));
        clock.presented(2, now, Duration::ZERO);
        assert_eq!(
            clock.deadline(now).unwrap().0,
            now + Duration::from_millis(12)
        );
    }

    #[test]
    fn predicts_across_periods_without_past_deadlines() {
        let start = Instant::now();
        let mut clock = FrameClock::default();
        clock.presented(0, start, Duration::from_millis(16));
        for ms in 0..1000 {
            let now = start + Duration::from_millis(ms);
            let (latch, target) = clock.deadline(now).unwrap();
            assert!(latch >= now);
            assert!(latch < now + Duration::from_millis(16));
            assert_eq!(target.duration_since(start).as_millis() % 16, 0);
        }
    }

    #[test]
    fn cost_is_a_decaying_max_excluding_present() {
        let mut clock = FrameClock::default();
        clock.submitted(0, None, Duration::from_millis(10));
        clock.submitted(1, None, Duration::from_millis(1));
        assert_eq!(clock.cost, Duration::from_millis(9));
        clock.submitted(2, None, Duration::from_millis(12));
        assert_eq!(clock.cost, Duration::from_millis(12));
    }

    #[test]
    fn constant_pipeline_lag_is_a_hit_even_with_subperiod_jitter() {
        let now = Instant::now();
        let period = Duration::from_millis(16);
        let mut clock = FrameClock::default();
        for seq in 0..100 {
            clock.submitted(seq, Some(now), Duration::ZERO);
            clock.presented(seq, now + period * 2 + Duration::from_millis(7), period);
            assert!(clock.margin <= Duration::from_millis(4));
        }
        assert_eq!(clock.margin, Duration::from_millis(1));
    }

    #[test]
    fn extra_periods_grow_margin_hits_shrink_and_discards_forget_targets() {
        let now = Instant::now();
        let period = Duration::from_millis(16);
        let mut clock = FrameClock::default();
        clock.submitted(0, Some(now), Duration::ZERO);
        clock.presented(0, now + period * 2, period);
        assert_eq!(clock.margin, Duration::from_micros(3900));
        clock.submitted(1, Some(now), Duration::ZERO);
        clock.presented(1, now + period * 3, Duration::ZERO);
        assert_eq!(clock.margin, Duration::from_micros(4900));
        clock.submitted(2, Some(now), Duration::ZERO);
        clock.discarded(2);
        clock.presented(2, now + period * 2, period);
        assert_eq!(clock.margin, Duration::from_micros(4900));
        assert!(clock.targets.is_empty());
        for seq in 3..200 {
            clock.submitted(seq, Some(now), Duration::ZERO);
            clock.presented(seq, now + period * (2 + seq as u32 % 2), period);
        }
        assert_eq!(clock.margin, period);
        for seq in 200..500 {
            clock.submitted(seq, Some(now), Duration::ZERO);
            clock.presented(seq, now + period * 2, period);
        }
        assert_eq!(clock.margin, Duration::from_millis(1));
    }

    #[test]
    fn recent_min_adapts_to_deeper_and_shallower_pipelines_and_refresh_changes() {
        let now = Instant::now();
        let period = Duration::from_millis(16);
        let mut clock = FrameClock::default();
        for seq in 0..=LAG_WINDOW {
            clock.submitted(seq, Some(now), Duration::ZERO);
            clock.presented(seq, now + period * if seq == 0 { 2 } else { 3 }, period);
        }
        assert_eq!(*clock.lags.iter().min().unwrap(), 3);
        let margin = clock.margin;
        clock.submitted(100, Some(now), Duration::ZERO);
        clock.presented(100, now + period * 3, period);
        assert_eq!(clock.margin, margin - Duration::from_micros(100));
        clock.submitted(101, Some(now), Duration::ZERO);
        clock.presented(101, now + period, period);
        assert_eq!(*clock.lags.iter().min().unwrap(), 1);
        let margin = clock.margin;
        clock.submitted(102, Some(now), Duration::ZERO);
        clock.presented(102, now + period * 2, period);
        assert_eq!(
            clock.margin,
            (margin + Duration::from_millis(1)).min(period)
        );
        clock.submitted(103, Some(now), Duration::ZERO);
        clock.presented(103, now + period * 10, period * 2);
        assert_eq!(clock.lags, VecDeque::from([5]));
        assert!(clock.margin <= period);
    }
}
