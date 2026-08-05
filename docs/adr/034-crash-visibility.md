# ADR 034 — A crash leaves evidence, and the two kinds are told apart

- **Status:** Accepted
- **Date:** 2026-08-05
- **Feature brief:** [Session Log and Crash Visibility](../features/session-log-and-crash-visibility.md)
- **Relates to:** ADR 028 (plugin host recording commands), ADR 030 (recording
  completeness accounting)

## Context

Augur exited mid-run during an unattended A1 protocol survey on Windows and left
nothing behind. No message, no file, no clue which of the survey's rows it died
on. That is not a gap in the report — it is the accurate consequence of the
program's design:

- there was no logging of any kind: no `tracing`, no `log`, no subscriber, no
  file. `main` was a single call into the render backend.
- there was no panic hook. A panic printed to stderr, and for a build started by
  double-clicking on Windows stderr is a console that is destroyed with the
  process. The message is displayed for as long as it takes the window to close.
- a hard crash printed nothing anywhere, because nothing ran.

The diagnostics that did exist were all *inside* the running program —
`PipelineStatsSnapshot`, the GUI diagnostics pane, `last_error`. They describe a
program that is still alive to describe itself. None of them survive it.

This is the same principle the recording path already commits to (ADR 030,
"unavoidable loss must be measured, never silent"), applied to the program's own
death rather than to its data.

## Decision

**Write a session log, and treat the two failure classes as the different things
they are.**

A panic and a hard crash are not variants of one event. A panic unwinds, so Rust
code can run and write a full report: message, source location, backtrace. A
hard crash — access violation, `abort`, stack overflow, OOM, a kill, a power
cut — runs no further Rust code at all. **No hook can catch it, because there is
nothing left to run the hook.** Any design that tries to handle both with one
mechanism silently handles only the first.

So:

1. **Panics** are recorded by a hook that *delegates to the previous hook* after
   writing. Terminal behaviour is unchanged; nothing is lost, only added.
2. **Hard crashes are detected after the fact, by absence.** Startup writes a
   breadcrumb file; a clean exit deletes it. A breadcrumb present at the next
   start is proof the previous session never reached the end of `main`. The
   entry is written then and the breadcrumb consumed.

**A `CRASH` entry with no `PANIC` above it is a positive finding, not a missing
one.** It says the death involved no Rust-level fault, which is what sends the
next question to the OS — on Windows, the Application event log's exception code
and faulting module. Collapsing the two into one "something went wrong" line
would destroy exactly that distinction.

**The breadcrumb carries an activity line**, overwritten as the program works.
For an unattended survey this is the difference between "it crashed sometime
overnight" and "it crashed recording row 23 of 40". Plugin recordings set it,
and a plugin that reports its own position in its recording metadata
(`protocol_point_index` / `protocol_point_total`) gets that position recorded.

**The log lives in per-user state**, not beside the measurement: it must exist
before an output folder is chosen and survive choosing another. `AUGUR_LOG_DIR`
overrides it. The path is resolved from environment variables directly rather
than through `dirs`, which is an **optional** dependency of `augur-gui` —
crash reporting must not be switchable off by a feature flag.

**The path is shown in the GUI**, under Diagnostics, with a copy button. The
startup message announcing it goes to the same stderr the crash message went
to — a diagnostic nobody can find is not a diagnostic.

## Consequences

- A crash is now diagnosable after the fact instead of only reproducible.
- The next crash of the kind that prompted this is classifiable within seconds:
  `PANIC` means read the backtrace, `CRASH` alone means go to the event log.
- Cost is one small file write per session and one per activity change (a
  handful per recording), all outside the capture path.
- The log grows without bound. Sessions are one line each and panics are rare,
  so this is left until it is a problem worth a rotation policy.
- A breadcrumb is per-pid, so two concurrent instances do not report each other
  as crashed. A stale breadcrumb from a *running* instance would be misreported,
  which cannot happen for the single-instance bench use this serves.
- **This records deaths; it does not prevent them.** The crash that prompted the
  ADR is still unexplained. What changes is that the next one will not be.
