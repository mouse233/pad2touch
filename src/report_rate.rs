//! Observed host delivery and PTP scan cadence, never a claimed bus polling rate.
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Default)]
pub struct Snapshot {
    pub raw_hz: Option<f64>,
    pub frame_hz: Option<f64>,
    pub scan_hz: Option<f64>,
}
impl Snapshot {
    pub fn label(self) -> String {
        let show = |v: Option<f64>| v.map(|n| format!("{n:.1}")).unwrap_or_else(|| "--".into());
        format!(
            "Observed Hz: raw {} / frames {} (host); scan {}",
            show(self.raw_hz),
            show(self.frame_hz),
            show(self.scan_hz)
        )
    }
}
#[derive(Default)]
pub struct Meter {
    raw: VecDeque<Instant>,
    frames: VecDeque<Instant>,
    scan_deltas: VecDeque<(Instant, u16)>,
    last_scan: Option<(Instant, u16)>,
}
const GAP: Duration = Duration::from_millis(250);
fn add(queue: &mut VecDeque<Instant>, now: Instant) {
    if queue
        .back()
        .is_some_and(|&last| now.duration_since(last) > GAP)
    {
        queue.clear();
    }
    queue.push_back(now);
    while queue.len() > 8192
        || queue
            .front()
            .is_some_and(|&first| now.duration_since(first) > Duration::from_secs(1))
    {
        queue.pop_front();
    }
}
fn rate(queue: &VecDeque<Instant>, now: Instant) -> Option<f64> {
    let (first, last) = (*queue.front()?, *queue.back()?);
    let span = last.duration_since(first).as_secs_f64();
    (now.duration_since(last) <= GAP && span >= 0.1 && queue.len() >= 4)
        .then(|| (queue.len() - 1) as f64 / span)
}
impl Meter {
    pub fn raw(&mut self, now: Instant) {
        add(&mut self.raw, now);
    }
    pub fn frame(&mut self, now: Instant, scan: Option<u32>, active: bool) {
        add(&mut self.frames, now);
        if !active {
            self.last_scan = None;
            self.scan_deltas.clear();
            return;
        }
        let Some(scan) = scan.filter(|n| *n <= u16::MAX as u32).map(|n| n as u16) else {
            self.last_scan = None;
            self.scan_deltas.clear();
            return;
        };
        if let Some((last, old)) = self.last_scan {
            let delta = scan.wrapping_sub(old);
            if now.duration_since(last) <= GAP && (1..=2500).contains(&delta) {
                self.scan_deltas.push_back((now, delta));
            } else {
                self.scan_deltas.clear();
            }
        }
        self.last_scan = Some((now, scan));
        while self.scan_deltas.len() > 8192
            || self
                .scan_deltas
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(1))
        {
            self.scan_deltas.pop_front();
        }
    }
    pub fn snapshot(&self, now: Instant) -> Snapshot {
        let scan_hz = if self.scan_deltas.len() >= 8
            && self
                .last_scan
                .is_some_and(|(at, _)| now.duration_since(at) <= GAP)
        {
            let mut deltas: Vec<_> = self.scan_deltas.iter().map(|(_, d)| *d).collect();
            deltas.sort_unstable();
            let n = deltas.len();
            let median = if n % 2 == 1 {
                deltas[n / 2] as f64
            } else {
                (deltas[n / 2 - 1] as f64 + deltas[n / 2] as f64) / 2.0
            };
            Some(10000.0 / median)
        } else {
            None
        };
        Snapshot {
            raw_hz: rate(&self.raw, now),
            frame_hz: rate(&self.frames, now),
            scan_hz,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragments_and_scan_wrap_do_not_inflate_complete_frame_rate() {
        let start = Instant::now();
        let mut meter = Meter::default();
        for i in 0..30 {
            let at = start + Duration::from_millis(i * 8);
            for _ in 0..5 {
                meter.raw(at);
            }
            meter.frame(at, Some((65500u32 + i as u32 * 80) & 0xffff), true);
        }
        let s = meter.snapshot(start + Duration::from_millis(232));
        assert!((s.frame_hz.unwrap() - 125.0).abs() < 1e-6);
        assert_eq!(s.scan_hz, Some(125.0));
        assert!(s.raw_hz.unwrap() > 600.0);
        let idle = meter.snapshot(start + Duration::from_secs(2));
        assert!(idle.frame_hz.is_none());
        assert!(idle.scan_hz.is_none());
        meter.frame(start + Duration::from_secs(2), Some(0), true);
        assert!(meter
            .snapshot(start + Duration::from_secs(2))
            .scan_hz
            .is_none());
    }
}
