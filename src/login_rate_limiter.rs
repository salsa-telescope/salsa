//! Per-IP rate limiter for failed logins.
//!
//! Failures are recorded as they happen and checked separately, so a
//! successful login can clear the record — the policy is "ten bad attempts in
//! five minutes locks you out until they age off, unless you get one right".
//! The counting itself lives in [`IpEventLog`].

use std::net::IpAddr;
use std::time::Duration;

use crate::ip_event_log::IpEventLog;

const MAX_FAILURES: usize = 10;
const WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct LoginRateLimiterHandle {
    failures: IpEventLog,
}

impl Default for LoginRateLimiterHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginRateLimiterHandle {
    pub fn new() -> Self {
        Self {
            failures: IpEventLog::new(WINDOW),
        }
    }

    /// Returns true if the IP is currently blocked.
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        self.failures.count(ip) >= MAX_FAILURES
    }

    /// Record a failed login attempt for this IP.
    pub fn record_failure(&self, ip: IpAddr) {
        self.failures.record(ip);
    }

    /// Clear the failure record for this IP on successful login.
    pub fn record_success(&self, ip: IpAddr) {
        self.failures.clear(ip);
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
    fn not_blocked_with_fewer_than_max_failures() {
        let limiter = LoginRateLimiterHandle::new();
        let ip = ip(1);
        for _ in 0..MAX_FAILURES - 1 {
            limiter.record_failure(ip);
        }
        assert!(!limiter.is_blocked(ip));
    }

    #[test]
    fn blocked_at_max_failures() {
        let limiter = LoginRateLimiterHandle::new();
        let ip = ip(2);
        for _ in 0..MAX_FAILURES {
            limiter.record_failure(ip);
        }
        assert!(limiter.is_blocked(ip));
    }

    #[test]
    fn success_clears_block() {
        let limiter = LoginRateLimiterHandle::new();
        let ip = ip(3);
        for _ in 0..MAX_FAILURES {
            limiter.record_failure(ip);
        }
        assert!(limiter.is_blocked(ip));
        limiter.record_success(ip);
        assert!(!limiter.is_blocked(ip));
    }

    #[test]
    fn different_ips_are_independent() {
        let limiter = LoginRateLimiterHandle::new();
        let ip_a = ip(4);
        let ip_b = ip(5);
        for _ in 0..MAX_FAILURES {
            limiter.record_failure(ip_a);
        }
        assert!(limiter.is_blocked(ip_a));
        assert!(!limiter.is_blocked(ip_b));
    }
}
