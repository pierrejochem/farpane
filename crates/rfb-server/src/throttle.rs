//! Delays for addresses that keep failing to sign in (design doc, section 20:
//! "failures per IP address add increasing delays").

use std::{
    collections::HashMap,
    net::IpAddr,
    time::{Duration, Instant},
};

/// Failures older than this are forgotten.
const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);
/// The longest delay.
const MAX_DELAY: Duration = Duration::from_secs(30);
/// Addresses remembered at most; a flood of new ones cannot grow memory.
const MAX_TRACKED: usize = 4096;

/// Counts failed sign-ins per address and says how long the next attempt from
/// that address has to wait: none after zero failures, then 1 s, 2 s, 4 s ...
/// up to 30 s.
#[derive(Debug, Default)]
pub struct Throttle {
    failures: HashMap<IpAddr, (u32, Instant)>,
    /// Connections open per address.
    open: HashMap<IpAddr, usize>,
}

/// What one client counts as: its own address, or its whole /64 for IPv6
/// (one customer typically holds a /64 and can pick any address in it).
fn key(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let mut octets = v6.octets();
    octets[8..].fill(0);
    IpAddr::from(octets)
}

impl Throttle {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How long a connection from `ip` has to wait at `now`.
    #[must_use]
    pub fn delay(&self, ip: IpAddr, now: Instant) -> Duration {
        match self.failures.get(&key(ip)) {
            Some(&(n, at)) if n > 0 && now.saturating_duration_since(at) < FORGET_AFTER => {
                // 2^(n-1) seconds, without overflowing for large n.
                let secs = 1u64 << (n - 1).min(10);
                Duration::from_secs(secs).min(MAX_DELAY)
            }
            _ => Duration::ZERO,
        }
    }

    /// Admit a connection from `ip`, or `None` when it already has `max_open`.
    /// The caller must call [`Throttle::leave`] when the connection ends.
    pub fn enter(&mut self, ip: IpAddr, max_open: usize) -> bool {
        let open = self.open.entry(key(ip)).or_insert(0);
        if *open >= max_open {
            return false;
        }
        *open += 1;
        true
    }

    /// A connection admitted by [`Throttle::enter`] ended.
    pub fn leave(&mut self, ip: IpAddr) {
        let k = key(ip);
        if let Some(open) = self.open.get_mut(&k) {
            *open -= 1;
            if *open == 0 {
                self.open.remove(&k);
            }
        }
    }

    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        let ip = key(ip);
        if self.failures.len() >= MAX_TRACKED && !self.failures.contains_key(&ip) {
            self.failures
                .retain(|_, &mut (_, at)| now.saturating_duration_since(at) < FORGET_AFTER);
            if self.failures.len() >= MAX_TRACKED {
                // Still full of live entries: drop the oldest.
                if let Some(oldest) = self
                    .failures
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(ip, _)| *ip)
                {
                    self.failures.remove(&oldest);
                }
            }
        }
        let entry = self.failures.entry(ip).or_insert((0, now));
        if now.saturating_duration_since(entry.1) >= FORGET_AFTER {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    /// A successful sign-in clears the address.
    pub fn record_success(&mut self, ip: IpAddr) {
        self.failures.remove(&key(ip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 0, 2, last])
    }

    #[test]
    fn delays_double_up_to_the_cap_and_are_per_address() {
        let mut t = Throttle::new();
        let now = Instant::now();
        assert_eq!(t.delay(ip(1), now), Duration::ZERO);
        let mut seen = Vec::new();
        for _ in 0..8 {
            t.record_failure(ip(1), now);
            seen.push(t.delay(ip(1), now).as_secs());
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(
            t.delay(ip(2), now),
            Duration::ZERO,
            "other addresses are unaffected"
        );
    }

    #[test]
    fn failures_are_forgotten_after_a_while_and_after_success() {
        let mut t = Throttle::new();
        let now = Instant::now();
        t.record_failure(ip(1), now);
        t.record_failure(ip(1), now);
        assert_eq!(t.delay(ip(1), now + FORGET_AFTER), Duration::ZERO);
        // A failure after the old ones expired starts counting again.
        t.record_failure(ip(1), now + FORGET_AFTER);
        assert_eq!(t.delay(ip(1), now + FORGET_AFTER), Duration::from_secs(1));
        t.record_success(ip(1));
        assert_eq!(t.delay(ip(1), now + FORGET_AFTER), Duration::ZERO);
    }

    #[test]
    fn an_ipv6_prefix_counts_as_one_address() {
        let mut t = Throttle::new();
        let now = Instant::now();
        let a: IpAddr = "2001:db8::1".parse().unwrap();
        let b: IpAddr = "2001:db8::ffff:2".parse().unwrap();
        let other: IpAddr = "2001:db8:0:1::1".parse().unwrap();
        t.record_failure(a, now);
        assert_eq!(t.delay(b, now), Duration::from_secs(1));
        assert_eq!(t.delay(other, now), Duration::ZERO);
    }

    #[test]
    fn open_connections_per_address_are_capped_and_given_back() {
        let mut t = Throttle::new();
        assert!(t.enter(ip(1), 2));
        assert!(t.enter(ip(1), 2));
        assert!(!t.enter(ip(1), 2));
        assert!(t.enter(ip(2), 2), "other addresses are unaffected");
        t.leave(ip(1));
        assert!(t.enter(ip(1), 2));
    }

    #[test]
    fn memory_stays_bounded() {
        let mut t = Throttle::new();
        let now = Instant::now();
        for i in 0..u32::try_from(MAX_TRACKED * 2).unwrap() {
            t.record_failure(IpAddr::from(i.to_be_bytes()), now);
        }
        assert!(t.failures.len() <= MAX_TRACKED);
    }
}
