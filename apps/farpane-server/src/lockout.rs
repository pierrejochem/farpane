//! Lock a user name after repeated failures (design doc, section 20: 5
//! failed sign-ins in 15 minutes lock that user for 15 minutes).

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Failures that lock a name.
pub const MAX_FAILURES: usize = 5;
/// The window in which the failures count.
pub const WINDOW: Duration = Duration::from_secs(15 * 60);
/// How long a locked name stays locked.
pub const LOCK: Duration = Duration::from_secs(15 * 60);
/// Names remembered at most.
const MAX_TRACKED: usize = 4096;

#[derive(Debug, Default)]
struct Record {
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
}

/// Failure history per user name. Unknown names are tracked too, so the lock
/// does not reveal which accounts exist.
#[derive(Debug, Default)]
pub struct Lockout {
    names: HashMap<String, Record>,
}

fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Lockout {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// If `name` is locked at `now`, until when.
    #[must_use]
    pub fn locked_until(&self, name: &str, now: Instant) -> Option<Instant> {
        self.names
            .get(&key(name))
            .and_then(|r| r.locked_until)
            .filter(|&until| until > now)
    }

    /// Record a failed sign-in. Returns `true` if this one locked the name.
    pub fn record_failure(&mut self, name: &str, now: Instant) -> bool {
        if self.names.len() >= MAX_TRACKED && !self.names.contains_key(&key(name)) {
            self.names.retain(|_, r| {
                r.locked_until.is_some_and(|u| u > now)
                    || r.failures
                        .back()
                        .is_some_and(|&t| now.duration_since(t) < WINDOW)
            });
            if self.names.len() >= MAX_TRACKED {
                // Still full: a flood of made-up names. Forget the names that
                // are only counting failures, never one that is locked.
                self.names
                    .retain(|_, r| r.locked_until.is_some_and(|u| u > now));
            }
            if self.names.len() >= MAX_TRACKED {
                if let Some(any) = self.names.keys().next().cloned() {
                    self.names.remove(&any);
                }
            }
        }
        let record = self.names.entry(key(name)).or_default();
        while record
            .failures
            .front()
            .is_some_and(|&t| now.saturating_duration_since(t) >= WINDOW)
        {
            record.failures.pop_front();
        }
        record.failures.push_back(now);
        if record.failures.len() >= MAX_FAILURES && record.locked_until.is_none_or(|u| u <= now) {
            record.locked_until = Some(now + LOCK);
            record.failures.clear();
            return true;
        }
        false
    }

    /// A successful sign-in forgets the failures.
    pub fn record_success(&mut self, name: &str) {
        self.names.remove(&key(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_failures_lock_for_fifteen_minutes() {
        let mut l = Lockout::new();
        let t0 = Instant::now();
        for i in 0..4 {
            assert!(!l.record_failure("alice", t0 + Duration::from_secs(i)));
            assert_eq!(l.locked_until("alice", t0), None);
        }
        assert!(l.record_failure("alice", t0 + Duration::from_secs(4)));
        assert!(
            l.locked_until("alice", t0 + Duration::from_secs(5))
                .is_some()
        );
        assert!(
            l.locked_until("ALICE", t0 + Duration::from_secs(5))
                .is_some(),
            "ignores case"
        );
        assert!(l.locked_until("bob", t0 + Duration::from_secs(5)).is_none());
        // Locked until 15 minutes after the fifth failure, not a second less.
        let until = t0 + Duration::from_secs(4) + LOCK;
        let just_before = until.checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(l.locked_until("alice", just_before), Some(until));
        assert_eq!(l.locked_until("alice", until), None);
    }

    #[test]
    fn slow_failures_do_not_add_up() {
        let mut l = Lockout::new();
        let t0 = Instant::now();
        for i in 0..20u64 {
            // One failure every 4 minutes: never 5 within 15 minutes.
            assert!(!l.record_failure("alice", t0 + Duration::from_secs(i * 240)));
        }
    }

    #[test]
    fn success_clears_the_count() {
        let mut l = Lockout::new();
        let t0 = Instant::now();
        for _ in 0..4 {
            l.record_failure("alice", t0);
        }
        l.record_success("alice");
        assert!(!l.record_failure("alice", t0));
    }

    #[test]
    fn memory_stays_bounded() {
        let mut l = Lockout::new();
        let t0 = Instant::now();
        for i in 0..(MAX_TRACKED * 2) {
            l.record_failure(&format!("user{i}"), t0);
        }
        assert!(l.names.len() <= MAX_TRACKED);
    }
}
