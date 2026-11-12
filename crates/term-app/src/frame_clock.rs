use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub struct FrameClock {
    last: Option<Instant>,
    period: Duration,
    cost: Duration,
    margin: Duration,
    targets: BTreeMap<usize, Instant>,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self {
            last: None,
            period: Duration::ZERO,
            cost: Duration::ZERO,
            margin: Duration::from_millis(4),
            targets: BTreeMap::new(),
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
            self.period = refresh;
        }
        if let Some(target) = self.targets.remove(&seq)
            && !self.period.is_zero()
        {
            self.margin = if time.saturating_duration_since(target) >= self.period {
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
    fn misses_grow_margin_hits_shrink_and_discards_forget_targets() {
        let now = Instant::now();
        let period = Duration::from_millis(16);
        let mut clock = FrameClock::default();
        clock.submitted(0, Some(now), Duration::ZERO);
        clock.presented(0, now + period, period);
        assert_eq!(clock.margin, Duration::from_millis(5));
        clock.submitted(1, Some(now + period), Duration::ZERO);
        clock.presented(1, now + period, Duration::ZERO);
        assert_eq!(clock.margin, Duration::from_micros(4900));
        clock.submitted(2, Some(now), Duration::ZERO);
        clock.discarded(2);
        clock.presented(2, now + period * 2, period);
        assert_eq!(clock.margin, Duration::from_micros(4900));
        assert!(clock.targets.is_empty());
        for seq in 3..100 {
            clock.submitted(seq, Some(now), Duration::ZERO);
            clock.presented(seq, now + period, period);
        }
        assert_eq!(clock.margin, period);
        for seq in 100..400 {
            clock.submitted(seq, Some(now + period), Duration::ZERO);
            clock.presented(seq, now + period, period);
        }
        assert_eq!(clock.margin, Duration::from_millis(1));
    }
}
