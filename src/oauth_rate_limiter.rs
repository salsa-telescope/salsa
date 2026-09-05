//! Per-IP rate limiter for OAuth2 login starts.
//!
//! Every hit on `/auth/redirect/{provider}` writes a `pending_oauth2` row
//! before the visitor is sent off to the provider, and the endpoint needs no
//! authentication to reach. Without a limit an attacker can grow that table as
//! fast as SQLite accepts inserts — which costs disk, and contends for the one
//! shared connection every other request has to queue behind.
//!
//! The window is deliberately the same length as `OAUTH2_PENDING_LIFETIME_SECS`:
//! a pending row lives exactly that long, so capping starts per window caps how
//! many *live* rows one address can be responsible for at any moment. That
//! bounds the table by construction rather than by how promptly the hourly
//! sweep runs.
//!
//! The cap is generous on purpose, because the two errors are not symmetric.
//! Refusing a real visitor costs them their way in; letting one through costs a
//! row of a few dozen bytes that deletes itself. School groups reach this site
//! from behind a single NAT address — thirty students clicking "log in" at once
//! is an ordinary afternoon here, not an attack — so the threshold is set where
//! only automation finds it.
//!
//! Like [`crate::guest_rate_limiter`] and unlike [`crate::login_rate_limiter`],
//! nothing clears the count: a start is consumed the moment it is allowed, so
//! the check and the record are one atomic step.

use std::net::IpAddr;
use std::time::Duration;

use crate::ip_event_log::IpEventLog;
use crate::models::session::OAUTH2_PENDING_LIFETIME_SECS;

const MAX_STARTS_PER_WINDOW: usize = 100;
const WINDOW: Duration = Duration::from_secs(OAUTH2_PENDING_LIFETIME_SECS as u64);

#[derive(Clone)]
pub struct OauthStartLimiterHandle {
    starts: IpEventLog,
}

impl Default for OauthStartLimiterHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl OauthStartLimiterHandle {
    pub fn new() -> Self {
        Self {
            starts: IpEventLog::new(WINDOW),
        }
    }

    /// If `ip` is at the limit, returns `true` and records nothing. Otherwise
    /// records the attempt and returns `false`. The handler should treat a
    /// `true` return as "refuse this login start".
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
        let limiter = OauthStartLimiterHandle::new();
        for _ in 0..MAX_STARTS_PER_WINDOW - 1 {
            assert!(!limiter.check_and_record(ip(1)));
        }
    }

    #[test]
    fn at_limit_returns_true() {
        let limiter = OauthStartLimiterHandle::new();
        for _ in 0..MAX_STARTS_PER_WINDOW {
            assert!(!limiter.check_and_record(ip(2)));
        }
        assert!(limiter.check_and_record(ip(2)));
    }

    #[test]
    fn different_ips_are_independent() {
        let limiter = OauthStartLimiterHandle::new();
        for _ in 0..MAX_STARTS_PER_WINDOW {
            assert!(!limiter.check_and_record(ip(3)));
        }
        assert!(limiter.check_and_record(ip(3)));
        assert!(!limiter.check_and_record(ip(4)));
    }

    /// A class behind one NAT address must not lock itself out. Thirty
    /// students, each fumbling the flow twice, is well inside the cap.
    #[test]
    fn a_school_group_behind_one_address_is_not_refused() {
        let limiter = OauthStartLimiterHandle::new();
        for _ in 0..90 {
            assert!(!limiter.check_and_record(ip(5)));
        }
    }

    /// The window matches the lifetime of the row each start creates, so the
    /// cap doubles as a bound on how many live rows an address can hold.
    #[test]
    fn window_matches_the_pending_oauth2_lifetime() {
        assert_eq!(WINDOW.as_secs() as i64, OAUTH2_PENDING_LIFETIME_SECS);
    }
}
