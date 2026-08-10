//! Per-IP sliding-window event counter.
//!
//! The shared mechanism behind [`crate::login_rate_limiter`] and
//! [`crate::guest_rate_limiter`]: both count how many times an IP did
//! something within a window, and both prune entries that have aged out. Only
//! the policy on top differs — failed logins are recorded and cleared on
//! success, guest starts are consumed atomically — so this type deliberately
//! exposes the counting primitive rather than a "is it allowed" verdict, and
//! each limiter keeps its own semantics.
//!
//! Best-effort and in-memory: a process restart forgets everything, which is
//! acceptable for both callers.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct IpEventLog {
    window: Duration,
    inner: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
}

impl IpEventLog {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Number of events recorded for `ip` inside the window.
    ///
    /// Read-only in the map sense: an IP that has never been recorded is not
    /// inserted. That matters because this is called on every login attempt,
    /// and inserting here would grow the map by one permanent entry per IP
    /// that ever probes the login endpoint.
    pub fn count(&self, ip: IpAddr) -> usize {
        let map = self.inner.lock().unwrap();
        let now = Instant::now();
        map.get(&ip)
            .map(|timestamps| {
                timestamps
                    .iter()
                    .filter(|t| now.duration_since(**t) < self.window)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Record one event for `ip`, pruning any that have aged out.
    ///
    /// Entries that prune to empty are dropped rather than left as empty
    /// vectors, so an IP that goes quiet stops costing anything.
    pub fn record(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let timestamps = map.entry(ip).or_default();
        timestamps.retain(|t| now.duration_since(*t) < self.window);
        timestamps.push(now);
    }

    /// Forget everything recorded for `ip`.
    pub fn clear(&self, ip: IpAddr) {
        self.inner.lock().unwrap().remove(&ip);
    }

    /// Record an event unless `ip` is already at `max` within the window.
    /// Returns `true` when at the limit, in which case nothing is recorded.
    pub fn record_unless_at(&self, ip: IpAddr, max: usize) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let timestamps = map.entry(ip).or_default();
        timestamps.retain(|t| now.duration_since(*t) < self.window);
        if timestamps.len() >= max {
            return true;
        }
        timestamps.push(now);
        false
    }

    #[cfg(test)]
    fn tracked_ips(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, a))
    }

    fn log() -> IpEventLog {
        IpEventLog::new(Duration::from_secs(300))
    }

    #[test]
    fn counts_are_per_ip() {
        let log = log();
        log.record(ip(1));
        log.record(ip(1));
        log.record(ip(2));
        assert_eq!(log.count(ip(1)), 2);
        assert_eq!(log.count(ip(2)), 1);
    }

    #[test]
    fn clear_forgets_one_ip_only() {
        let log = log();
        log.record(ip(1));
        log.record(ip(2));
        log.clear(ip(1));
        assert_eq!(log.count(ip(1)), 0);
        assert_eq!(log.count(ip(2)), 1);
    }

    #[test]
    fn events_outside_the_window_do_not_count() {
        let log = IpEventLog::new(Duration::from_millis(1));
        log.record(ip(1));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(log.count(ip(1)), 0);
    }

    // Counting must not be what grows the map: `is_blocked` runs on every
    // login attempt, so an insert here would leave one permanent entry per
    // IP that ever probes the endpoint.
    #[test]
    fn counting_an_unseen_ip_does_not_track_it() {
        let log = log();
        assert_eq!(log.count(ip(1)), 0);
        assert_eq!(log.tracked_ips(), 0);
    }

    #[test]
    fn record_unless_at_stops_recording_at_the_limit() {
        let log = log();
        for _ in 0..3 {
            assert!(!log.record_unless_at(ip(1), 3));
        }
        assert!(log.record_unless_at(ip(1), 3));
        // The refused call must not have been counted.
        assert_eq!(log.count(ip(1)), 3);
    }
}
