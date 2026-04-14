use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_FAILURES: usize = 10;
const WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub struct LoginRateLimiterHandle {
    inner: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
}

impl Default for LoginRateLimiterHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginRateLimiterHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns true if the IP is currently blocked.
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock().unwrap();
        let now = Instant::now();
        let timestamps = map.entry(ip).or_default();
        timestamps.retain(|t| now.duration_since(*t) < WINDOW);
        timestamps.len() >= MAX_FAILURES
    }

    /// Record a failed login attempt for this IP.
    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap();
        map.entry(ip).or_default().push(Instant::now());
    }

    /// Clear the failure record for this IP on successful login.
    pub fn record_success(&self, ip: IpAddr) {
        let mut map = self.inner.lock().unwrap();
        map.remove(&ip);
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
