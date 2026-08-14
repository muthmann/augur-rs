# Session Log and Crash Visibility

Augur writes an append-only session log so a run that ends badly leaves
evidence. Before this, it wrote nothing at all: no subscriber, no panic hook, no
file. An unattended survey that died overnight was indistinguishable from one
that was never started.

See [ADR 034](../adr/034-crash-visibility.md) for why it is built this way.

## Where the log is

| platform | path |
|---|---|
| Windows | `%LOCALAPPDATA%\augur\logs\sessions.log` |
| macOS | `~/Library/Application Support/augur/logs/sessions.log` |
| Linux | `$XDG_STATE_HOME/augur/logs/sessions.log` (or `~/.local/state/...`) |

`AUGUR_LOG_DIR` overrides it. The resolved path is printed to stderr at startup
and shown — with a **Copy path** button — under **Diagnostics** in the viewer
panel, because a build started by double-clicking has no stderr anyone can read.

It is per-user state, not per-measurement: the log has to exist before an output
folder has been chosen and has to survive choosing a different one.

## What is recorded

```
2026-08-05T21:14:02Z START  pid 8124 · augur 1.0.0 · windows x86_64
2026-08-05T21:14:19Z EXIT   clean
2026-08-05T23:02:41Z START  pid 9330 · augur 1.0.0 · windows x86_64
2026-08-06T04:11:08Z CRASH  the previous session did not exit cleanly — no panic
                            was recorded, so it ended without unwinding (access
                            violation, abort, stack overflow, out of memory,
                            kill or power loss)
                            pid 9330 · started 2026-08-05T23:02:41Z · augur 1.0.0
                            while: recording automated-run_p23
```

| entry | meaning |
|---|---|
| `START` | a session began — pid, version, OS, architecture |
| `EXIT   clean` | `main` returned; the session ended in an orderly way |
| `PANIC` | a Rust panic: message, source location, the activity, and a backtrace |
| `CRASH` | reported at the *next* start: the previous session never reached the end of `main` |

## Why `CRASH` is found late

The two failure classes need different mechanisms, and no single one catches
both.

A **panic** unwinds, so a hook can run and write a full report. A **hard
crash** — access violation, `abort`, stack overflow, OOM, a kill, a power cut —
runs no further Rust code at all. Nothing can be written at the moment it
happens.

So it is detected afterwards. Startup writes a breadcrumb file
(`session-<pid>.open`) and a clean exit deletes it. A breadcrumb still on disk
at the next start is proof that the session it belongs to never got to the end
of `main`. The entry is written then, and the breadcrumb is consumed so one
crash is not re-reported forever.

**`CRASH` and `PANIC` mean genuinely different things.** A panic is a bug in
Rust code with a location attached. A `CRASH` with no `PANIC` above it is
something no Rust code was involved in — the place to look next is the OS. On
Windows:

```powershell
Get-WinEvent -FilterHashtable @{LogName='Application'; Id=1000,1001} -MaxEvents 20 |
  Where-Object { $_.Message -match 'augur' } | Format-List TimeCreated, Message
```

which names the exception code and the faulting module: `0xC0000005` (access
violation), `0xC00000FD` (stack overflow) and `0xC0000409` (abort) point in
completely different directions.

## The activity line

The breadcrumb carries one line saying what the program was doing, so a crash is
pinned to a point in the work rather than to a time of night. It is overwritten,
not accumulated — the question is "what was it doing when it died".

Plugin-driven recordings set it from their generic run ID. The host does not
interpret plugin metadata or know whether a run represents a protocol row,
survey, or another workflow. A plugin can include useful position information
in its run ID. Between recordings it reads `idle between plugin recordings`, so
a death *during* a recording is distinguishable from one *between* two of them
— a different fault with a different cause.
