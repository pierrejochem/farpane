//! Sign-in for `X509Plain`: the user list, the lockout and the event log
//! behind one verifier.

use std::{
    sync::{Arc, Condvar, Mutex, RwLock},
    time::Instant,
};

use rfb_server::Permission;
use rfb_tokio::PlainVerifier;

use crate::{lockout::Lockout, users::UserStore};

/// Checks sign-ins against a [`UserStore`], locking names that fail too
/// often, and reports what happened through `log`.
#[derive(Clone)]
pub struct Authenticator {
    users: Arc<RwLock<UserStore>>,
    lockout: Arc<Mutex<Lockout>>,
    log: Arc<dyn Fn(String) + Send + Sync>,
    gate: Arc<HashGate>,
}

impl std::fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Authenticator")
    }
}

impl Authenticator {
    #[must_use]
    pub fn new(users: UserStore, log: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            users: Arc::new(RwLock::new(users)),
            lockout: Arc::new(Mutex::new(Lockout::new())),
            log: Arc::new(log),
            gate: Arc::new(HashGate::new()),
        }
    }

    /// The check `rfb-tokio` calls with the user name and password.
    #[must_use]
    pub fn verifier(&self) -> PlainVerifier {
        let this = self.clone();
        PlainVerifier::new(move |user, password| this.check(user, password))
    }

    /// What `user` may do once signed in.
    #[must_use]
    pub fn permission(&self, user: Option<&str>) -> Permission {
        let users = self
            .users
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match user.and_then(|u| users.get(u)) {
            Some(u) if u.enabled && u.role.may_control() => Permission::Full,
            // Unknown (cannot happen after a sign-in, but an edit may have
            // removed the user meanwhile) and view-only both look only.
            _ => Permission::ViewOnly,
        }
    }

    fn check(&self, user: &[u8], password: &[u8]) -> bool {
        let Ok(name) = std::str::from_utf8(user) else {
            (self.log)("sign-in refused: user name is not UTF-8".to_owned());
            return false;
        };
        let now = Instant::now();
        // Count the attempt as a failure *before* the slow check and undo it
        // on success. Checking first and recording afterwards would let any
        // number of parallel guesses pass the lock check while the first
        // ones are still being hashed.
        let (locked, now_locked) = {
            let mut lockout = self
                .lockout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if lockout.locked_until(name, now).is_some() {
                (true, false)
            } else {
                (false, lockout.record_failure(name, now))
            }
        };
        // Pick up edits made with `farpane-server user ...` while running.
        self.users
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reload_if_changed();
        // Always do the expensive check, so a locked name takes as long as
        // any other and the right password cannot be told from the answer.
        let role = {
            let _slot = self.gate.enter();
            self.users
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .verify(name, password)
        };
        if locked {
            (self.log)(format!(
                "sign-in refused for {name:?}: the account is locked"
            ));
            return false;
        }
        if role.is_some() {
            self.lockout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_success(name);
            true
        } else {
            (self.log)(format!("sign-in failed for {name:?}"));
            if now_locked {
                (self.log)(format!(
                    "{name:?} is locked for 15 minutes after repeated failures"
                ));
            }
            false
        }
    }
}

/// Lets at most [`MAX_HASHES`] password checks run at once. Argon2id takes
/// tens of megabytes and some tens of milliseconds, so without a bound a
/// crowd of unauthenticated connections could exhaust memory.
#[derive(Debug)]
struct HashGate {
    running: Mutex<usize>,
    freed: Condvar,
}

/// Most password hashes computed at the same time.
const MAX_HASHES: usize = 4;

/// Held while one hash runs.
struct HashSlot<'a>(&'a HashGate);

impl HashGate {
    fn new() -> Self {
        Self {
            running: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Wait for a free slot (the caller is on a blocking thread).
    fn enter(&self) -> HashSlot<'_> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *running >= MAX_HASHES {
            running = self
                .freed
                .wait(running)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *running += 1;
        HashSlot(self)
    }
}

impl Drop for HashSlot<'_> {
    fn drop(&mut self) {
        *self
            .0
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        self.0.freed.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::users::Role;

    fn setup() -> (tempfile::TempDir, Authenticator, Arc<StdMutex<Vec<String>>>) {
        let dir = tempfile::tempdir().unwrap();
        let mut users = UserStore::load(&dir.path().join("users.toml")).unwrap();
        users
            .add("alice", "", b"alice's pass", Role::Admin)
            .unwrap();
        users.add("viewer", "", b"viewer pass", Role::View).unwrap();
        let log = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let auth = Authenticator::new(users, move |line| sink.lock().unwrap().push(line));
        (dir, auth, log)
    }

    #[test]
    fn right_and_wrong_passwords_and_the_permission_of_each_role() {
        let (_d, auth, _log) = setup();
        assert!(auth.check(b"alice", b"alice's pass"));
        assert!(!auth.check(b"alice", b"nope nope"));
        assert!(!auth.check(b"nobody", b"alice's pass"));
        assert!(!auth.check(&[0xFF, 0xFE], b"alice's pass"));
        assert_eq!(auth.permission(Some("alice")), Permission::Full);
        assert_eq!(auth.permission(Some("viewer")), Permission::ViewOnly);
        assert_eq!(auth.permission(Some("nobody")), Permission::ViewOnly);
        assert_eq!(auth.permission(None), Permission::ViewOnly);
    }

    #[test]
    fn five_failures_lock_even_the_right_password_out_and_are_logged() {
        let (_d, auth, log) = setup();
        for _ in 0..5 {
            assert!(!auth.check(b"alice", b"wrong pass"));
        }
        assert!(!auth.check(b"alice", b"alice's pass"), "locked");
        assert!(
            auth.check(b"viewer", b"viewer pass"),
            "other users are unaffected"
        );
        let log = log.lock().unwrap().join("\n");
        assert!(log.contains("locked for 15 minutes"), "{log}");
        assert!(log.contains("the account is locked"), "{log}");
        assert!(
            !log.contains("alice's pass") && !log.contains("wrong pass"),
            "no passwords in the log"
        );
    }

    #[test]
    fn unknown_names_lock_too_so_the_lock_reveals_nothing() {
        let (_d, auth, log) = setup();
        for _ in 0..6 {
            assert!(!auth.check(b"ghost", b"whatever pass"));
        }
        assert!(log.lock().unwrap().join("\n").contains("account is locked"));
    }

    #[test]
    fn parallel_guesses_cannot_get_past_the_lock() {
        let (_d, auth, _log) = setup();
        let handles: Vec<_> = (0..12)
            .map(|_| {
                let auth = auth.clone();
                std::thread::spawn(move || auth.check(b"alice", b"wrong pass"))
            })
            .collect();
        for h in handles {
            assert!(!h.join().unwrap());
        }
        // Twelve attempts at once must have locked the name, so now even the
        // right password is refused.
        assert!(!auth.check(b"alice", b"alice's pass"));
    }
}
