//! Per-IP rate limiter for guest-session starts.
//!
//! Worst-case abuse with no limiter: a single IP automates a guest start
//! every few seconds and locks every telescope in rotation, which on a
//! 3-dish system would block legitimate visitors entirely. Five starts per
//! hour is comfortable headroom for genuine "click once to try, give up,
//! try again later" behaviour, while making spam pointless.
//!
//! Unlike `login_rate_limiter` there is nothing that clears the count: a
//! start is consumed the moment it is allowed, so the check and the record
//! are one atomic step. The counting itself lives in [`IpEventLog`].

use std::net::IpAddr;
use std::time::Duration;

use crate::ip_event_log::IpEventLog;

const MAX_STARTS_PER_WINDOW: usize = 5;
const WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct GuestStartLimiterHandle {
    starts: IpEventLog,
}

impl Default for GuestStartLimiterHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl GuestStartLimiterHandle {
    pub fn new() -> Self {
        Self {
            starts: IpEventLog::new(WINDOW),
        }
    }

    /// If `ip` is at the limit, returns `true` and records nothing.
    /// Otherwise records the attempt and returns `false`. The handler
    /// should treat a `true` return as "refuse this start".
    pub fn check_and_record(&self, ip: IpAddr) -> bool {
        self.starts.record_unless_at(ip, MAX_STARTS_PER_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, a))
    }

    #[test]
    fn under_limit_returns_false() {
        let limiter = GuestStartLimiterHandle::new();
        let ip = ip(1);
        for _ in 0..MAX_STARTS_PER_WINDOW - 1 {
            assert!(!limiter.check_and_record(ip));
        }
    }

    #[test]
    fn at_limit_returns_true() {
        let limiter = GuestStartLimiterHandle::new();
        let ip = ip(2);
        for _ in 0..MAX_STARTS_PER_WINDOW {
            assert!(!limiter.check_and_record(ip));
        }
        // The next call is over the limit.
        assert!(limiter.check_and_record(ip));
    }

    #[test]
    fn different_ips_are_independent() {
        let limiter = GuestStartLimiterHandle::new();
        let ip_a = ip(3);
        let ip_b = ip(4);
        for _ in 0..MAX_STARTS_PER_WINDOW {
            assert!(!limiter.check_and_record(ip_a));
        }
        assert!(limiter.check_and_record(ip_a));
        // ip_b should still be under its own limit.
        assert!(!limiter.check_and_record(ip_b));
    }
}
