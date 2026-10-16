//! `farpane-server user add | passwd | role | disable | enable | remove | list`.

use std::{io, io::Write, path::Path};

use crate::users::{Role, UserStore};

/// The text of `farpane-server user --help`.
pub const USAGE: &str = "\
Usage: farpane-server user COMMAND [--config-dir DIR]

  add NAME [--role admin|control|view] [--display-name TEXT]
                      create a user; the first one is an admin by default
  passwd NAME         change a password
  role NAME ROLE      change a role (admin, control or view)
  disable NAME        forbid signing in, keep the account
  enable NAME
  remove NAME
  list

The password is read from $FARPANE_PASSWORD, from --password-file FILE, or as
one line from standard input. It is never taken from the command line. The
last enabled admin cannot be removed, disabled or demoted without --force.";

/// Run a `user` command. `args` are the words after `user`; `password` is
/// asked only for commands that need one.
///
/// # Errors
/// A message for the user.
pub fn run(
    args: &[String],
    users_file: &Path,
    password: &mut dyn FnMut() -> io::Result<String>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let mut words: Vec<&str> = args.iter().map(String::as_str).collect();
    let force = take_flag(&mut words, "--force");
    let role = take_value(&mut words, "--role")?;
    let display = take_value(&mut words, "--display-name")?;
    let mut store = UserStore::load(users_file).map_err(|e| e.to_string())?;
    let say =
        |out: &mut dyn Write, text: String| writeln!(out, "{text}").map_err(|e| e.to_string());
    let [command, rest @ ..] = words.as_slice() else {
        return Err(USAGE.to_owned());
    };
    // The last enabled admin must stay: otherwise nobody can manage the rest.
    let guard = |store: &UserStore, name: &str, what: &str| -> Result<(), String> {
        let is_admin = store
            .get(name)
            .is_some_and(|u| u.enabled && u.role == Role::Admin);
        let admins = store
            .users()
            .iter()
            .filter(|u| u.enabled && u.role == Role::Admin)
            .count();
        if is_admin && admins == 1 && !force {
            return Err(format!(
                "{name:?} is the last enabled admin; {what} them with --force if you mean it"
            ));
        }
        Ok(())
    };
    match (*command, rest) {
        ("add", [name]) => {
            let role = match role {
                Some(r) => Role::parse(&r).ok_or("the role must be admin, control or view")?,
                None if store.users().is_empty() => Role::Admin,
                None => Role::Control,
            };
            let pw = password().map_err(|e| format!("reading the password: {e}"))?;
            store
                .add(name, display.as_deref().unwrap_or(""), pw.as_bytes(), role)
                .map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("added {name} ({role})"))
        }
        ("passwd", [name]) => {
            let pw = password().map_err(|e| format!("reading the password: {e}"))?;
            store
                .set_password(name, pw.as_bytes())
                .map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("password changed for {name}"))
        }
        ("role", [name, role]) => {
            let role = Role::parse(role).ok_or("the role must be admin, control or view")?;
            if role != Role::Admin {
                guard(&store, name, "demote")?;
            }
            store.set_role(name, role).map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("{name} is now {role}"))
        }
        ("disable", [name]) => {
            guard(&store, name, "disable")?;
            store.set_enabled(name, false).map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("{name} disabled"))
        }
        ("enable", [name]) => {
            store.set_enabled(name, true).map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("{name} enabled"))
        }
        ("remove", [name]) => {
            guard(&store, name, "remove")?;
            store.remove(name).map_err(|e| e.to_string())?;
            store.save().map_err(|e| e.to_string())?;
            say(out, format!("{name} removed"))
        }
        ("list", []) => {
            if store.users().is_empty() {
                return say(
                    out,
                    "no users yet: add one with `farpane-server user add NAME`".into(),
                );
            }
            for u in store.users() {
                say(
                    out,
                    format!(
                        "{:<20} {:<8} {:<9} {}",
                        u.name,
                        u.role,
                        if u.enabled { "enabled" } else { "disabled" },
                        u.display_name
                    ),
                )?;
            }
            Ok(())
        }
        ("help" | "--help" | "-h", _) => say(out, USAGE.to_owned()),
        _ => Err(USAGE.to_owned()),
    }
}

fn take_flag(words: &mut Vec<&str>, flag: &str) -> bool {
    let before = words.len();
    words.retain(|w| *w != flag);
    words.len() != before
}

fn take_value(words: &mut Vec<&str>, flag: &str) -> Result<Option<String>, String> {
    let Some(i) = words.iter().position(|w| *w == flag) else {
        return Ok(None);
    };
    words.remove(i);
    if i >= words.len() {
        return Err(format!("{flag} needs a value"));
    }
    Ok(Some(words.remove(i).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_in(dir: &Path, args: &[&str], pw: &str) -> Result<String, String> {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        let pw = pw.to_owned();
        run(
            &args,
            &dir.join("users.toml"),
            &mut || Ok(pw.clone()),
            &mut out,
        )?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[test]
    fn the_first_user_is_an_admin_and_later_ones_can_control() {
        let dir = tempfile::tempdir().unwrap();
        run_in(dir.path(), &["add", "root-user"], "first password").unwrap();
        run_in(
            dir.path(),
            &["add", "bob", "--display-name", "Bob B."],
            "bobs password",
        )
        .unwrap();
        run_in(
            dir.path(),
            &["add", "eve", "--role", "view"],
            "eves password",
        )
        .unwrap();
        let list = run_in(dir.path(), &["list"], "").unwrap();
        let lines: Vec<&str> = list.lines().collect();
        assert!(
            lines[0].contains("root-user") && lines[0].contains("admin"),
            "{list}"
        );
        assert!(
            lines[1].contains("bob") && lines[1].contains("control") && lines[1].contains("Bob B."),
            "{list}"
        );
        assert!(
            lines[2].contains("eve") && lines[2].contains("view"),
            "{list}"
        );
    }

    #[test]
    fn the_last_admin_is_protected() {
        let dir = tempfile::tempdir().unwrap();
        run_in(dir.path(), &["add", "boss"], "boss password").unwrap();
        for args in [
            &["remove", "boss"][..],
            &["disable", "boss"],
            &["role", "boss", "view"],
        ] {
            let err = run_in(dir.path(), args, "").unwrap_err();
            assert!(err.contains("last enabled admin"), "{args:?}: {err}");
        }
        // With a second admin it is fine; --force overrides.
        run_in(
            dir.path(),
            &["add", "deputy", "--role", "admin"],
            "deputy password",
        )
        .unwrap();
        run_in(dir.path(), &["disable", "boss"], "").unwrap();
        assert!(run_in(dir.path(), &["remove", "deputy"], "").is_err());
        run_in(dir.path(), &["remove", "deputy", "--force"], "").unwrap();
    }

    #[test]
    fn passwords_change_and_errors_are_plain() {
        let dir = tempfile::tempdir().unwrap();
        run_in(dir.path(), &["add", "ann"], "old password").unwrap();
        run_in(dir.path(), &["passwd", "ann"], "new password").unwrap();
        let store = UserStore::load(&dir.path().join("users.toml")).unwrap();
        assert!(store.verify("ann", b"new password").is_some());
        assert!(store.verify("ann", b"old password").is_none());
        assert!(
            run_in(dir.path(), &["passwd", "ghost"], "whatever pw")
                .unwrap_err()
                .contains("no user")
        );
        assert!(
            run_in(dir.path(), &["add", "ann"], "another pw 1")
                .unwrap_err()
                .contains("already exists")
        );
        assert!(
            run_in(dir.path(), &["add", "zed"], "short")
                .unwrap_err()
                .contains("at least 8")
        );
        assert!(
            run_in(
                dir.path(),
                &["add", "zed", "--role", "root"],
                "long enough pw"
            )
            .unwrap_err()
            .contains("role must be")
        );
        assert!(
            run_in(dir.path(), &["add", "zed", "--role"], "long enough pw")
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            run_in(dir.path(), &["frobnicate"], "")
                .unwrap_err()
                .contains("Usage")
        );
    }

    #[test]
    fn a_failed_command_does_not_touch_the_file() {
        let dir = tempfile::tempdir().unwrap();
        run_in(dir.path(), &["add", "ann"], "old password").unwrap();
        let before = std::fs::read_to_string(dir.path().join("users.toml")).unwrap();
        let _ = run_in(dir.path(), &["add", "bad name"], "long enough pw");
        let _ = run_in(dir.path(), &["remove", "ann"], "");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("users.toml")).unwrap(),
            before
        );
    }
}
