//! What the program leaves behind when it stops.
//!
//! Augur had no logging of any kind: no subscriber, no panic hook, no file. A
//! panic printed to a stderr that, for a double-clicked `.exe` on Windows, is a
//! console that closes with the process — the message flashes and is gone. A
//! hard crash (access violation, `abort`, stack overflow, the OOM killer) never
//! printed anything at all. Either way nothing reached the disk, so an
//! unattended run that died overnight left no evidence that it had run.
//!
//! This module writes that evidence. It covers the two classes separately,
//! because no single mechanism catches both:
//!
//! - **Panics** are caught by a hook, which records the message, the source
//!   location and a backtrace. The previous hook still runs, so behaviour in a
//!   terminal is unchanged.
//! - **Hard crashes cannot be caught at all** — there is no unwinding and no
//!   Rust code left to run. They are detected *afterwards*, by a breadcrumb:
//!   a file written at startup and deleted on a clean exit. Finding one at the
//!   next start proves the previous session did not reach the end of `main`,
//!   which is the only in-process evidence such a death can leave.
//!
//! The breadcrumb also carries a one-line note of what the program was doing
//! ([`note`]). For an unattended protocol survey that is the difference between
//! "it crashed sometime overnight" and "it crashed recording row 23".
//!
//! Everything lands in one append-only file so the sessions read in order.
//! `AUGUR_LOG_DIR` overrides the location; [`log_path`] reports it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::{Mutex, OnceLock};

use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const APP_DIR: &str = "augur";
const LOG_FILE: &str = "sessions.log";

/// The live session's breadcrumb, kept so the panic hook and [`note`] can find
/// it without threading a handle through the whole GUI.
static SESSION: OnceLock<Mutex<Session>> = OnceLock::new();
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

struct Session {
    breadcrumb: PathBuf,
    started_at: String,
    activity: String,
}

impl Session {
    /// Rewrites the breadcrumb. Its whole purpose is to survive a death that
    /// runs no further code, so it is written through to the file every time
    /// rather than buffered.
    fn persist(&self) {
        let body = format!(
            "pid = {}\nstarted_at = {}\nversion = {}\nactivity = {}\n",
            process::id(),
            self.started_at,
            env!("CARGO_PKG_VERSION"),
            self.activity
        );
        let _ = fs::write(&self.breadcrumb, body);
    }
}

/// Where the session log lives, once [`install`] has run.
pub fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH.get()
}

/// Installs crash reporting. Call once, first thing in `main`.
///
/// Returns the log path, so the caller can say where it is — a diagnostic
/// nobody can find is not a diagnostic.
pub fn install() -> Option<PathBuf> {
    let dir = log_directory()?;
    if let Err(error) = fs::create_dir_all(&dir) {
        // Nothing to log *to*, so this is the one message that has to go to
        // stderr and be accepted as possibly unseen.
        eprintln!("augur: cannot create the log directory {dir:?}: {error}");
        return None;
    }
    let log = dir.join(LOG_FILE);
    let _ = LOG_PATH.set(log.clone());

    // Before claiming this session, account for the ones that never finished.
    report_unclean_sessions(&dir);

    let started_at = now();
    let session = Session {
        breadcrumb: dir.join(format!("session-{}.open", process::id())),
        started_at: started_at.clone(),
        activity: "starting up".into(),
    };
    session.persist();
    append(&format!(
        "START  pid {} · augur {} · {} {}",
        process::id(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ));
    let _ = SESSION.set(Mutex::new(session));

    install_panic_hook();
    Some(log)
}

/// Records what the program is doing, in one short line.
///
/// Overwrites the previous note rather than accumulating: the question a
/// breadcrumb answers is "what was it doing when it died", not "what did it do
/// all night" — that is what the log is for.
pub fn note(activity: impl Into<String>) {
    let activity = activity.into();
    if let Some(session) = SESSION.get() {
        if let Ok(mut session) = session.lock() {
            session.activity = activity;
            session.persist();
        }
    }
}

/// Records the end of a clean run and removes the breadcrumb, so the next
/// start does not report this session as a crash.
///
/// Only reached when `main` returns — which is exactly the point.
pub fn mark_clean_exit() {
    let Some(session) = SESSION.get() else {
        return;
    };
    let Ok(session) = session.lock() else {
        return;
    };
    let _ = fs::remove_file(&session.breadcrumb);
    append("EXIT   clean");
}

/// Adds the crash report to whatever the previous hook did, rather than
/// replacing it: in a terminal the default message is still the fastest way to
/// see what happened, and this is meant to lose nothing.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown location".into());
        let message = panic_message(info);
        // `force_capture` rather than `capture`, which is a no-op unless
        // RUST_BACKTRACE is set — an operator cannot be expected to have
        // exported it before the crash they did not know was coming.
        let backtrace = std::backtrace::Backtrace::force_capture();
        let activity = SESSION
            .get()
            // A panic raised *while* the session lock is held would deadlock on
            // `lock()`. The report is worth more than the activity line.
            .and_then(|session| session.try_lock().ok().map(|s| s.activity.clone()))
            .unwrap_or_else(|| "unknown".into());
        append(&format!(
            "PANIC  {message}\n       at {location}\n       while: {activity}\n{backtrace}"
        ));
        previous(info);
    }));
}

fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "panic with a non-string payload".into()
    }
}

/// Turns every breadcrumb left on disk into a log entry, then clears it.
///
/// A breadcrumb that is still here at startup belongs to a process that never
/// reached `mark_clean_exit`. That is a hard crash, a kill, or a power loss —
/// the three deaths that leave nothing else behind.
fn report_unclean_sessions(dir: &PathBuf) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut stale: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "open")
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("session-"))
        })
        .collect();
    stale.sort();
    for path in stale {
        let body = fs::read_to_string(&path).unwrap_or_default();
        let field = |key: &str| {
            body.lines()
                .find_map(|line| line.strip_prefix(&format!("{key} = ")))
                .unwrap_or("unknown")
                .to_owned()
        };
        append(&format!(
            "CRASH  the previous session did not exit cleanly — no panic was \
             recorded, so it ended without unwinding (access violation, abort, \
             stack overflow, out of memory, kill or power loss)\n       \
             pid {} · started {} · augur {}\n       while: {}",
            field("pid"),
            field("started_at"),
            field("version"),
            field("activity"),
        ));
        let _ = fs::remove_file(&path);
    }
}

fn append(entry: &str) {
    let Some(path) = LOG_PATH.get() else {
        return;
    };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = writeln!(file, "{} {entry}", now());
}

fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown-time".into())
}

/// Per-user state, not per-measurement: the log has to exist before an output
/// folder has been chosen, and has to survive choosing a different one.
///
/// Resolved by hand rather than through `dirs`, which is an optional dependency
/// of this crate — crash reporting must not be switched off by a feature flag.
fn log_directory() -> Option<PathBuf> {
    if let Some(override_dir) = std::env::var_os("AUGUR_LOG_DIR") {
        return Some(PathBuf::from(override_dir));
    }
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
    };
    Some(
        base.unwrap_or_else(std::env::temp_dir)
            .join(APP_DIR)
            .join("logs"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The breadcrumb has to be readable by the *next* process, which shares no
    /// memory with this one — so its shape is part of the contract.
    #[test]
    fn a_breadcrumb_round_trips_through_the_unclean_session_report() {
        let dir = std::env::temp_dir().join(format!("augur-diag-{}", process::id()));
        let _ = fs::create_dir_all(&dir);
        let session = Session {
            breadcrumb: dir.join("session-4242.open"),
            started_at: "2026-08-05T21:00:00Z".into(),
            activity: "recording row 23".into(),
        };
        session.persist();

        let body = fs::read_to_string(&session.breadcrumb).expect("breadcrumb");
        assert!(body.contains("started_at = 2026-08-05T21:00:00Z"), "{body}");
        assert!(body.contains("activity = recording row 23"), "{body}");
        // The pid is the *owning process's*, not the file name's: the file name
        // only has to be unique, the field has to identify the process that
        // died so it can be matched against the Windows event log.
        assert!(body.contains(&format!("pid = {}", process::id())), "{body}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The whole point: a death that runs no Rust code still turns into a line
    /// an operator can read, naming what was being recorded when it happened.
    ///
    /// Owns `LOG_PATH` for the test binary — it is a `OnceLock`, so no other
    /// test may set it.
    #[test]
    fn a_hard_crash_becomes_a_log_entry_naming_the_row_it_died_on() {
        let dir = std::env::temp_dir().join(format!("augur-diag-log-{}", process::id()));
        let _ = fs::create_dir_all(&dir);
        let log = dir.join(LOG_FILE);
        LOG_PATH.set(log.clone()).expect("only setter of LOG_PATH");

        // A session that started, got as far as row 23, and was never seen again.
        Session {
            breadcrumb: dir.join("session-4242.open"),
            started_at: "2026-08-05T21:00:00Z".into(),
            activity: "recording automated-run_p23 (protocol row 23/40)".into(),
        }
        .persist();
        report_unclean_sessions(&dir);

        let text = fs::read_to_string(&log).expect("session log");
        assert!(text.contains("CRASH"), "{text}");
        assert!(text.contains(&format!("pid {}", process::id())), "{text}");
        assert!(text.contains("started 2026-08-05T21:00:00Z"), "{text}");
        assert!(text.contains("protocol row 23/40"), "{text}");
        // Distinguishing it from a panic is the reason it is worth logging: the
        // two have different causes and different next steps.
        assert!(text.contains("without unwinding"), "{text}");
        // Consumed, so one crash is not re-reported at every later start.
        assert!(!dir.join("session-4242.open").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// An operator who has to guess where the log went does not have a log.
    #[test]
    fn the_log_directory_is_overridable_for_the_bench() {
        let expected = std::env::temp_dir().join("augur-log-override");
        // SAFETY: single-threaded test; no other thread reads the environment.
        unsafe { std::env::set_var("AUGUR_LOG_DIR", &expected) };
        assert_eq!(log_directory(), Some(expected));
        unsafe { std::env::remove_var("AUGUR_LOG_DIR") };
    }
}
