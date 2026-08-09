//! `cargo xtask clean-e2e-tmp` — reap stale e2e harness temp dirs (issue #322).
//!
//! The harness nests everything it creates under one `dad-tests-<pid>-*` root
//! per test process, removed by an `atexit` hook on the normal-exit path. A
//! process that is SIGKILLed — nextest's `slow-timeout terminate-after`, or an
//! interrupted run — never reaches that hook and leaves its root behind. On a
//! RAM-backed `/tmp` those leftovers are resident memory until someone notices,
//! and the failure mode is self-amplifying: the more the suite fails, the less
//! headroom the next run has.
//!
//! # Ownership decides, age is only the fallback (issue #461)
//!
//! The `<pid>` in the name is the process that created the root, so it answers
//! "is anyone still using this?" directly. Age is a proxy for the same question
//! and a bad one in both directions. Measured: 280 roots totalling 6.2 GB, every
//! one with a dead owning PID, were refused because the oldest was 4h09m and the
//! default threshold is 6h — on a 14 GB tmpfs with 5 MiB of swap left and an e2e
//! compile about to start. In the other direction, a genuine suite still running
//! past the threshold was *eligible* to have its own live root deleted out from
//! under it.
//!
//! So the PID decides wherever it can, and `--older-than` only filters the one
//! case it cannot settle. The decision is exactly three branches:
//!
//! - **dead PID** → reap, at any age. `--older-than` does not suppress it.
//! - **live PID** → keep, at any age, unconditionally.
//! - **no usable PID** — an untagged `.tmp*` dir, a pre-fix lock dir, a
//!   malformed name, or a platform with no `kill(2)` — the age rule decides.
//!
//! Liveness is `kill(pid, 0)`, in which `EPERM` counts as **alive**: the process
//! exists, it merely is not ours, and reading that as dead would delete a live
//! run's root.
//!
//! # Why there is no recycled-PID branch
//!
//! Issue #461 originally called for a fourth branch: a live PID *proven* to have
//! started after the root already existed is not the root's owner, so let age
//! decide. Two successive attempts to build that proof were wrong in the same
//! direction — deleting a live run's working directory — and the branch is
//! deliberately gone rather than patched a third time.
//!
//! The comparison is unsound in principle, not merely mis-implemented. Ordering
//! a process start against a directory timestamp has to bridge through the wall
//! clock: the directory side is only ever a stored `CLOCK_REALTIME` value, and
//! the process side has to be reconstructed as boot time plus a tick counter.
//! Linux's `getboottime64()` contract says outright that `settimeofday` shifts
//! the boot time behind `/proc/stat`'s `btime`, while inode timestamps are never
//! retroactively adjusted. So a forward clock step — admin action, a VM clock
//! correction, a time-sync daemon, suspend/resume — moves a *live* process's
//! reconstructed start forward while its root's timestamp stays put, and a
//! one-hour correction is enough to push a process that started a second before
//! creating its root an hour "after" it. The bias lands squarely on the
//! deletion-unsafe side, and no input available here turns it back into positive
//! proof. (The attempt before that read the `/proc/<pid>` directory's mtime,
//! which is a dentry *lookup* time — Linux instantiates that inode lazily — and
//! deleted live roots for a different reason with the same shape.)
//!
//! Dropping the branch costs almost nothing, because a PID collision is
//! **transient**. When a dead test's PID gets reused by an unrelated live
//! process, that root is kept for as long as the collision lasts — but the
//! colliding process eventually exits, and the next run classifies the root
//! `dead-pid` and reaps it. Nothing leaks permanently; reaping is merely
//! deferred. Trading a real risk of deleting a live e2e run against a deferral
//! is not a close call.
//!
//! Do not reinstate it, in any form — including a report-only "possibly
//! recycled" annotation. That would keep the whole `/proc` parsing surface,
//! which is exactly where the wrong answers came from, in exchange for a hint
//! nobody can act on differently.
//!
//! # What this will and will not delete
//!
//! Deleting by prefix in a shared `/tmp` is only safe for names this repo
//! actually owns:
//!
//! - `dad-tests-*` — the current harness root. Ours, unambiguously.
//! - `dot-agent-deck-test-lock-*` — the pre-fix lock dirs. Also ours; still
//!   present in bulk on machines that ran the suite before the leak was fixed.
//!   They carry no PID, so they are decided by age.
//! - `.tmp*` — **not** reaped unless `--include-untagged` is passed. That is
//!   the `tempfile` crate's *default* prefix, so it belongs to every Rust
//!   program on the machine, not just this suite. Globbing it blindly can
//!   delete a live temp dir owned by something else entirely.
//!
//! Dry-run is the default; `--apply` is required to remove anything.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

/// Directory-name prefixes this repo owns outright and may reap by default.
const OWNED_PREFIXES: &[&str] = &["dad-tests-", "dot-agent-deck-test-lock-"];

/// The prefix that carries the owning PID: `dad-tests-<pid>-<random>`.
const PID_TAGGED_PREFIX: &str = "dad-tests-";

/// The `tempfile` crate's default prefix — shared with every other Rust
/// program, so it is opt-in only.
const UNTAGGED_PREFIX: &str = ".tmp";

const DEFAULT_MAX_AGE_HOURS: u64 = 6;

/// Per-directory lines printed before the per-reason summary. A machine that
/// has been leaking for a few hours accumulates hundreds of roots, and 280
/// lines of path bury the one number the user needs; the summary below the list
/// is always complete, and the truncation says how much it dropped.
const MAX_LISTED: usize = 20;

/// Work budget for one root's size walk, in directory entries and in depth.
///
/// Before issue #461 the walk only ever ran on age-eligible roots. It now runs
/// on every owned root including fresh ones and live-owner ones that are about
/// to be kept, so an enormous — or deliberately planted — tree under a
/// world-writable `/tmp` would otherwise burn unbounded CPU and I/O on every
/// invocation, dry run included. Past the budget the walk stops and the size is
/// reported as a lower bound. Sizing is presentation only and never reaches
/// [`classify`], so truncating it cannot change a single reap/keep verdict.
const MAX_SIZE_WALK_ENTRIES: usize = 50_000;
const MAX_SIZE_WALK_DEPTH: usize = 64;

struct Options {
    max_age: Duration,
    apply: bool,
    include_untagged: bool,
}

/// What the PID embedded in a root's name says about who owns the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// A PID no live process holds: the root is definitively abandoned.
    Dead,
    /// A live PID. The root is kept, full stop — no timestamp of any kind is
    /// consulted, and there is deliberately no "but it might be recycled"
    /// escape hatch (see the module docs).
    Live,
    /// No PID to go on: an untagged or malformed name, or a platform on which
    /// liveness cannot be determined.
    Unknown,
}

/// Why a root was reaped or kept, so the report attributes each decision
/// instead of restating an age fact (issue #461).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    /// Owning process is gone — reaped whatever its age.
    DeadPid,
    /// Owning process is still running — kept whatever its age.
    LivePid,
    /// There was no usable PID, so the age rule decided.
    UntaggedAge,
}

impl Reason {
    /// Fixed order, so the summary reads the same way on every run.
    const ALL: [Reason; 3] = [Reason::DeadPid, Reason::LivePid, Reason::UntaggedAge];

    fn label(self) -> &'static str {
        match self {
            Reason::DeadPid => "dead-pid",
            Reason::LivePid => "live-pid",
            Reason::UntaggedAge => "untagged",
        }
    }

    /// One-line justification for the summary. The age-based reason needs the
    /// threshold and which side of it the dirs fell on; the PID-based ones are
    /// unconditional and say so.
    fn note(self, reap: bool, max_age: Duration) -> String {
        match self {
            Reason::DeadPid => "owning process is gone — reaped at any age".to_string(),
            Reason::LivePid => "owning process is still running — never reaped".to_string(),
            Reason::UntaggedAge => {
                let age = human_duration(max_age);
                let side = if reap { "older" } else { "younger" };
                format!("no owning PID in the name; {side} than {age}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Verdict {
    reap: bool,
    reason: Reason,
}

struct Candidate {
    path: PathBuf,
    bytes: u64,
    /// The size walk hit its budget, so `bytes` is a lower bound.
    size_truncated: bool,
    age: Duration,
    verdict: Verdict,
}

/// The one process fact the ownership decision needs, behind a trait so the
/// classification matrix can be driven from a table rather than from real PIDs.
///
/// The dead-PID test used to spawn a child, `wait()` it, and then probe the
/// number — but the kernel may reassign a PID the moment it is reaped, so under
/// PID churn that test observed an unrelated live process (issue #461 review).
/// Injecting the probe removes the race and makes every branch of [`owner_of`]
/// reachable without arranging a real process to match.
trait ProcessProbe {
    /// `Some(true)` alive, `Some(false)` dead, `None` where this platform
    /// cannot tell.
    fn is_alive(&self, pid: i32) -> Option<bool>;
}

/// The real probe: `kill(pid, 0)`.
struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn is_alive(&self, pid: i32) -> Option<bool> {
        pid_is_alive(pid)
    }
}

pub fn run(args: &[String]) -> ExitCode {
    let opts = match parse_args(args) {
        Ok(Some(opts)) => opts,
        Ok(None) => return ExitCode::SUCCESS, // --help
        Err(msg) => {
            eprintln!("xtask clean-e2e-tmp: {msg}");
            usage();
            return ExitCode::from(2);
        }
    };

    let temp_root = std::env::temp_dir();
    let outcome = match sweep(&temp_root, &opts, &SystemProbe) {
        Ok(outcome) => outcome,
        Err(e) => {
            eprintln!(
                "xtask clean-e2e-tmp: cannot read {}: {e}",
                temp_root.display()
            );
            return ExitCode::FAILURE;
        }
    };

    print!("{}", outcome.report);
    if outcome.removed > 0 || !outcome.failures.is_empty() {
        println!(
            "removed {} dir(s), freed {}",
            outcome.removed,
            human_size(outcome.freed, outcome.freed_truncated)
        );
    }
    for line in &outcome.failures {
        eprintln!("{line}");
    }
    if !outcome.failures.is_empty() {
        eprintln!("{} dir(s) could not be removed", outcome.failures.len());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn parse_args(args: &[String]) -> Result<Option<Options>, String> {
    let mut opts = Options {
        max_age: Duration::from_secs(DEFAULT_MAX_AGE_HOURS * 3600),
        apply: false,
        include_untagged: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--apply" => opts.apply = true,
            "--include-untagged" => opts.include_untagged = true,
            "--older-than" => {
                let raw = it
                    .next()
                    .ok_or_else(|| "--older-than needs a value in hours".to_string())?;
                let hours: u64 = raw
                    .parse()
                    .map_err(|_| format!("--older-than expects whole hours, got {raw:?}"))?;
                // `saturating_mul`: the value is user input, and an hour count
                // near `u64::MAX` would otherwise panic the whole command in a
                // debug build before it can clean anything. Saturating gives an
                // absurdly distant threshold, which is what such input means.
                opts.max_age = Duration::from_secs(hours.saturating_mul(3600));
            }
            "-h" | "--help" => {
                usage();
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Some(opts))
}

fn usage() {
    println!(
        "usage: cargo xtask clean-e2e-tmp [--older-than <hours>] [--apply] [--include-untagged]"
    );
    println!();
    println!("Reaps stale e2e harness temp dirs left by SIGKILLed test processes.");
    println!("Dry-run by default; --apply is required to delete.");
    println!();
    println!("A `{PID_TAGGED_PREFIX}<pid>-*` root is decided by whether that PID is still");
    println!("alive: dead means reaped at any age, alive means never reaped. The age");
    println!("threshold only decides roots with no usable PID — untagged or malformed");
    println!("names, and hosts with no way to ask whether a PID is alive.");
    println!();
    println!("  --older-than <hours>  age threshold for the fallback case only");
    println!(
        "                        (default: {DEFAULT_MAX_AGE_HOURS}). It does NOT hold back a dead PID."
    );
    println!("  --apply               actually remove the directories");
    println!("  --include-untagged    ALSO reap `{UNTAGGED_PREFIX}*` dirs. These use the");
    println!("                        tempfile crate's DEFAULT prefix and are shared with");
    println!("                        every Rust program on this machine — only use this");
    println!("                        when no other Rust build or tool is running.");
}

/// What one end-to-end pass over `temp_root` did.
struct Sweep {
    report: String,
    removed: usize,
    freed: u64,
    freed_truncated: bool,
    /// One message per directory that could not be removed; the caller decides
    /// where they go (stderr) and turns a non-empty list into a failing exit.
    failures: Vec<String>,
}

/// Collect, decide, report, and — only under `--apply` — delete.
///
/// This exists as one function so a test can drive the **real** deletion path
/// over a directory holding both a reapable and a kept root. `collect` returns
/// kept candidates too (the report has to explain survivors), which makes "only
/// the `reap` half is ever handed to `remove_dir_all`" the single
/// highest-consequence invariant in this file — and one that a test calling
/// `collect` alone can never observe.
fn sweep(temp_root: &Path, opts: &Options, probe: &dyn ProcessProbe) -> std::io::Result<Sweep> {
    let candidates = collect(temp_root, opts, probe)?;
    let (reap, keep): (Vec<Candidate>, Vec<Candidate>) =
        candidates.into_iter().partition(|c| c.verdict.reap);

    let mut out = report(temp_root, &reap, &keep, opts.max_age);
    let mut removed = 0usize;
    let mut freed = 0u64;
    let mut freed_truncated = false;
    let mut failures = Vec::new();

    if !reap.is_empty() {
        if opts.apply {
            for c in &reap {
                match std::fs::remove_dir_all(&c.path) {
                    Ok(()) => {
                        removed += 1;
                        // Saturating, like every other size accumulation here:
                        // apparent sizes are attacker-influenced (sparse files
                        // cost no blocks), and a panicking total would abort a
                        // sweep that has already deleted part of its work list.
                        freed = freed.saturating_add(c.bytes);
                        freed_truncated |= c.size_truncated;
                    }
                    // `{:?}` not `.display()`: see `report`.
                    Err(e) => failures.push(format!("  failed to remove {:?}: {e}", c.path)),
                }
            }
        } else {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "dry run — nothing removed. Re-run with --apply to delete."
            );
        }
    }

    Ok(Sweep {
        report: out,
        removed,
        freed,
        freed_truncated,
        failures,
    })
}

fn collect(
    temp_root: &Path,
    opts: &Options,
    probe: &dyn ProcessProbe,
) -> std::io::Result<Vec<Candidate>> {
    let now = SystemTime::now();
    let mut out = Vec::new();
    for entry in std::fs::read_dir(temp_root)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // `symlink_metadata` so a symlink is never mistaken for a directory —
        // a planted `dad-tests-*` symlink must not redirect the walk or the
        // removal outside the temp root.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_owned(name, opts.include_untagged) {
            continue;
        }
        // mtime is the only timestamp this tool reads, and it feeds the age
        // fallback alone. Ownership is decided from the PID with no timestamp
        // involved at all.
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        // Every owned dir is collected, kept ones included: the report has to
        // be able to say WHY a root survived, which it cannot do for entries
        // that were filtered away before they were ever seen.
        let verdict = classify(owner_of(name, probe), age, opts.max_age);
        let size = dir_size(&path);
        out.push(Candidate {
            bytes: size.bytes,
            size_truncated: size.truncated,
            path,
            age,
            verdict,
        });
    }
    // Biggest first. `Reverse` rather than a flipped `cmp` so
    // `unnecessary_sort_by` stays quiet under the workspace-wide clippy
    // gate (issue #436) — same ordering, same stability.
    out.sort_by_key(|c| std::cmp::Reverse(c.bytes));
    Ok(out)
}

fn is_owned(name: &str, include_untagged: bool) -> bool {
    if OWNED_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return true;
    }
    include_untagged && name.starts_with(UNTAGGED_PREFIX)
}

/// The whole reap/keep decision, kept free of the filesystem so it can be
/// exercised directly for every combination of ownership and age.
fn classify(owner: Owner, age: Duration, max_age: Duration) -> Verdict {
    match owner {
        // Definitive, and deliberately not subject to `--older-than`: this is
        // the case issue #461 was filed for, where 6.2 GB of provably-dead
        // roots were refused for being four hours old.
        Owner::Dead => Verdict {
            reap: true,
            reason: Reason::DeadPid,
        },
        // Unconditional, and the reason there is no timestamp in this function
        // beyond the age fallback: every attempt to qualify this branch with
        // "…unless the PID looks recycled" ended up deleting live roots.
        Owner::Live => Verdict {
            reap: false,
            reason: Reason::LivePid,
        },
        Owner::Unknown => Verdict {
            reap: age >= max_age,
            reason: Reason::UntaggedAge,
        },
    }
}

/// Classify a root from the PID in its name and nothing else.
///
/// No filesystem timestamp reaches this function. A live PID means keep, at any
/// age and whatever the root's timestamps say; see the module docs for why the
/// recycled-PID branch was removed rather than repaired.
fn owner_of(name: &str, probe: &dyn ProcessProbe) -> Owner {
    let Some(pid) = parse_pid(name) else {
        return Owner::Unknown;
    };
    match probe.is_alive(pid) {
        Some(false) => Owner::Dead,
        Some(true) => Owner::Live,
        // The platform cannot answer, so fall back to age exactly as an
        // untagged name would.
        None => Owner::Unknown,
    }
}

/// The `<pid>` out of a `dad-tests-<pid>-<random>` name. `None` for every other
/// shape, including the pre-fix lock dirs and untagged `.tmp*` names, which
/// carry no PID at all.
fn parse_pid(name: &str) -> Option<i32> {
    let rest = name.strip_prefix(PID_TAGGED_PREFIX)?;
    let digits = rest.split('-').next()?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // PID 0 is never a real process here, and `kill(0, 0)` addresses the whole
    // process group rather than one process — reject it rather than ask.
    digits.parse::<i32>().ok().filter(|pid| *pid > 0)
}

/// `Some(true)` alive, `Some(false)` dead, `None` where this platform cannot
/// tell.
///
/// `kill(pid, 0)` runs the existence and permission checks and sends no signal.
/// `ESRCH` is the ONLY answer that means dead: `EPERM` means the process exists
/// and simply is not ours, and reading that as dead would delete a live run's
/// root. Anything else unexpected is treated as alive for the same reason.
///
/// This is available on every Unix, not just Linux. Only a genuinely non-Unix
/// host — where there is no `kill(2)` to ask — returns `None` and falls back to
/// the age rule.
#[cfg(unix)]
fn pid_is_alive(pid: i32) -> Option<bool> {
    // SAFETY: `kill` with signal 0 sends nothing and touches no memory; `pid`
    // is a positive integer, so this can never address a process group.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return Some(true);
    }
    Some(std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: i32) -> Option<bool> {
    None
}

/// The human-facing report, built as a string so a test can assert that it
/// attributes each decision. Issue #461: the old output said "no owned dirs
/// older than 6h" — an age fact, when the question was ownership.
///
/// Candidate paths are rendered with `{:?}`, never `.display()`. `/tmp` is mode
/// 1777, so any local user can create a `dad-tests-<dead-pid>-<suffix>` whose
/// suffix carries a newline, a CSI sequence, or an OSC one the terminal may act
/// on (OSC 52 writes the clipboard) — and since the age floor no longer holds
/// anything back, such a name is reportable immediately, in the **default dry
/// run**. `Path`'s `Debug` escapes control characters; `Display` passes them
/// through verbatim.
fn report(temp_root: &Path, reap: &[Candidate], keep: &[Candidate], max_age: Duration) -> String {
    let mut out = String::new();
    if reap.is_empty() && keep.is_empty() {
        let _ = writeln!(
            out,
            "nothing to reap in {} (no dirs this repo owns)",
            temp_root.display()
        );
        return out;
    }

    for c in reap.iter().take(MAX_LISTED) {
        let _ = writeln!(
            out,
            "  {:>9}  {:<9} {:>4} old  {:?}",
            human_size(c.bytes, c.size_truncated),
            c.verdict.reason.label(),
            human_duration(c.age),
            c.path,
        );
    }
    if reap.len() > MAX_LISTED {
        let _ = writeln!(
            out,
            "  … and {} more (all counted in the summary below)",
            reap.len() - MAX_LISTED
        );
    }

    if reap.is_empty() {
        let _ = writeln!(out, "nothing to reap in {}", temp_root.display());
    } else {
        let _ = writeln!(
            out,
            "reap: {} dir(s), {} in {}",
            reap.len(),
            total_size(reap),
            temp_root.display(),
        );
        write_breakdown(&mut out, reap, true, max_age);
    }

    if !keep.is_empty() {
        let _ = writeln!(out, "keep: {} dir(s), {}", keep.len(), total_size(keep));
        write_breakdown(&mut out, keep, false, max_age);
    }

    if reap.iter().chain(keep).any(|c| c.size_truncated) {
        let _ = writeln!(
            out,
            "  sizes shown with ≥ are lower bounds: the walk stopped at {MAX_SIZE_WALK_ENTRIES} entries or depth {MAX_SIZE_WALK_DEPTH}. Size is reporting only — it never changes a reap/keep decision."
        );
    }
    out
}

/// Counts and sizes per reason — the part that stays readable when there are
/// 280 candidates.
fn write_breakdown(out: &mut String, cands: &[Candidate], reap: bool, max_age: Duration) {
    for reason in Reason::ALL {
        let matching: Vec<&Candidate> = cands
            .iter()
            .filter(|c| c.verdict.reason == reason)
            .collect();
        if matching.is_empty() {
            continue;
        }
        let bytes = sum_bytes(matching.iter().map(|c| c.bytes));
        let truncated = matching.iter().any(|c| c.size_truncated);
        let _ = writeln!(
            out,
            "  {:<9} {:>4} dir(s)  {:>9}  {}",
            reason.label(),
            matching.len(),
            human_size(bytes, truncated),
            reason.note(reap, max_age),
        );
    }
}

/// Summed size of a group, carrying the lower-bound marker if any member's walk
/// was truncated.
fn total_size(cands: &[Candidate]) -> String {
    human_size(
        sum_bytes(cands.iter().map(|c| c.bytes)),
        cands.iter().any(|c| c.size_truncated),
    )
}

/// Every size total in this file goes through here, and it saturates rather
/// than wrapping.
///
/// `Iterator::sum` panics on overflow in a debug build and wraps silently in a
/// release one, and these are *apparent* sizes: a sparse file costs no blocks,
/// so any local user can park three 8-exabyte files in a world-writable `/tmp`
/// under a `dad-tests-*` name and overflow a `u64`. The auditor did exactly
/// that and aborted the plain `cargo xtask clean-e2e-tmp` with `attempt to add
/// with overflow` before it could clean anything — which defeats the whole
/// point of a tool you reach for when the machine is already in trouble.
/// Saturating prints an implausible number; panicking or wrapping prints a
/// wrong one or nothing at all.
fn sum_bytes(sizes: impl Iterator<Item = u64>) -> u64 {
    sizes.fold(0u64, |acc, b| acc.saturating_add(b))
}

/// Apparent size of one tree, plus whether the walk gave up before finishing.
struct DirSize {
    bytes: u64,
    truncated: bool,
}

/// Recursive apparent size, bounded by [`MAX_SIZE_WALK_ENTRIES`] and
/// [`MAX_SIZE_WALK_DEPTH`].
///
/// # What the symlink handling does and does not guarantee
///
/// Every entry is stat'd with `symlink_metadata`, so **a symlink is never
/// descended as observed**: it contributes its own length and nothing more, and
/// a symlink loop cannot spin the walk. That is a statement about what the walk
/// *sees*, not a race-free guarantee, and the earlier flat claim that it "never
/// follows symlinks" was too absolute.
///
/// `read_dir` resolves by path. Between the moment a child is observed to be a
/// directory and the moment it is opened, a local user with write access inside
/// the tree can replace that path with a symlink, and the open follows the
/// replacement — so the *sizing* walk can be steered outside the candidate. The
/// `symlink_metadata` re-check below catches a swap that is still in place when
/// the directory is opened; a swap undone again inside that window is not
/// detected.
///
/// The residual consequence is bounded and presentation-only: at worst some
/// other tree's entries are counted into a size, capped by the entry and depth
/// budgets. No name and no content from outside the candidate is ever printed,
/// sizing never reaches [`classify`], so it cannot move a reap/keep verdict, and
/// it is not a deletion escape — `--apply` calls `remove_dir_all` on the
/// candidate path itself, which [`collect`] established was a directory and not
/// a symlink.
fn dir_size(path: &Path) -> DirSize {
    dir_size_bounded(path, MAX_SIZE_WALK_ENTRIES, MAX_SIZE_WALK_DEPTH)
}

fn dir_size_bounded(path: &Path, max_entries: usize, max_depth: usize) -> DirSize {
    let mut bytes = 0u64;
    let mut seen = 0usize;
    let mut truncated = false;
    let mut stack = vec![(path.to_path_buf(), 0usize)];
    'walk: while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        // The path was a real directory when it was queued, but `read_dir`
        // re-resolved it by name and would have followed a symlink swapped in
        // since. Re-stat before spending any of the budget here: it costs one
        // `lstat` per directory and rejects a swap that is still in place.
        if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        for entry in rd.flatten() {
            seen += 1;
            if seen > max_entries {
                truncated = true;
                break 'walk;
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                if depth + 1 > max_depth {
                    truncated = true;
                    continue;
                }
                stack.push((entry.path(), depth + 1));
            } else {
                // Saturating: see `sum_bytes`. `meta.len()` is the apparent
                // size, so three sparse files are enough to overflow a `u64`.
                bytes = bytes.saturating_add(meta.len());
            }
        }
    }
    DirSize { bytes, truncated }
}

/// A size, marked `≥` when the walk that produced it was cut short.
fn human_size(bytes: u64, truncated: bool) -> String {
    if truncated {
        format!("≥ {}", human_bytes(bytes))
    } else {
        human_bytes(bytes)
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn human_duration(d: Duration) -> String {
    let hours = d.as_secs() / 3600;
    if hours < 48 {
        format!("{hours}h")
    } else {
        format!("{}d", hours / 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_age: Duration) -> Options {
        Options {
            max_age,
            apply: false,
            include_untagged: false,
        }
    }

    const HOUR: Duration = Duration::from_secs(3600);

    fn candidate(name: &str, bytes: u64, age_hours: u32, verdict: Verdict) -> Candidate {
        Candidate {
            path: Path::new("/tmp").join(name),
            bytes,
            size_truncated: false,
            age: HOUR * age_hours,
            verdict,
        }
    }

    /// One scripted answer for every PID (issue #461 review, item 3). Real PIDs
    /// cannot express "dead" without a race — the kernel may hand a reaped
    /// number to someone else before the probe runs.
    struct FakeProbe(Option<bool>);

    impl FakeProbe {
        fn dead() -> Self {
            Self(Some(false))
        }
        fn live() -> Self {
            Self(Some(true))
        }
        fn unanswerable() -> Self {
            Self(None)
        }
    }

    impl ProcessProbe for FakeProbe {
        fn is_alive(&self, _pid: i32) -> Option<bool> {
            self.0
        }
    }

    /// A probe that answers per PID, for tests that need a reapable root and a
    /// kept root side by side in one directory.
    struct DeadPids(Vec<i32>);

    impl ProcessProbe for DeadPids {
        fn is_alive(&self, pid: i32) -> Option<bool> {
            Some(!self.0.contains(&pid))
        }
    }

    /// Force a directory's mtime, so a test can put a root's only timestamp
    /// anywhere on the wall clock — including the future, which no amount of
    /// waiting can produce.
    fn set_mtime(dir: &Path, when: SystemTime) {
        let handle = std::fs::File::open(dir).expect("open dir");
        handle
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("set mtime");
    }

    /// A file of `len` apparent bytes that occupies (almost) no blocks, so a
    /// test can build an exabyte-scale tree in milliseconds — the same trick
    /// available to any local user with write access to `/tmp`.
    fn sparse_file(path: &Path, len: u64) {
        std::fs::File::create(path)
            .expect("create sparse file")
            .set_len(len)
            .expect("set_len");
    }

    #[test]
    fn owned_prefixes_are_reaped_by_default() {
        assert!(is_owned("dad-tests-1234-AbCdEf", false));
        assert!(is_owned("dot-agent-deck-test-lock-AbCdEf", false));
    }

    /// The tempfile crate's default prefix belongs to every Rust program on the
    /// machine, so reaping it must stay opt-in — this is the guard against a
    /// prune helper deleting another tool's live temp dir.
    #[test]
    fn untagged_tempfile_prefix_is_opt_in() {
        assert!(!is_owned(".tmpAbCdEf", false));
        assert!(is_owned(".tmpAbCdEf", true));
    }

    #[test]
    fn unrelated_names_are_never_reaped() {
        for name in ["systemd-private-abc", "dad-screenshot.txt", "opencode"] {
            assert!(!is_owned(name, true), "{name} should not be reaped");
        }
    }

    /// A symlink named like an owned dir must not be collected — otherwise the
    /// reaper could be pointed at a tree outside the temp root.
    #[cfg(unix)]
    #[test]
    fn symlinks_named_like_owned_dirs_are_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("real-target");
        std::fs::create_dir(&target).expect("create target");
        std::os::unix::fs::symlink(&target, tmp.path().join("dad-tests-1-lnk"))
            .expect("create symlink");
        let found =
            collect(tmp.path(), &opts(Duration::ZERO), &FakeProbe::dead()).expect("collect");
        assert!(
            found.is_empty(),
            "symlink was collected: {:?}",
            found.iter().map(|c| c.path.clone()).collect::<Vec<_>>()
        );
        assert!(target.exists(), "target must be untouched");
    }

    /// Issue #461, requirement 1: the owning PID is read straight out of the
    /// name, and anything that is not that shape yields no PID at all.
    #[test]
    fn the_owning_pid_is_read_out_of_the_root_name() {
        assert_eq!(parse_pid("dad-tests-12345-AbCdEf"), Some(12345));
        assert_eq!(parse_pid("dad-tests-7-a-b-c"), Some(7));
        for name in [
            ".tmpAbCdEf",
            "dot-agent-deck-test-lock-AbCdEf",
            "dad-tests-",
            "dad-tests--AbCdEf",
            "dad-tests-notapid-AbCdEf",
            "dad-tests-12x-AbCdEf",
            "dad-tests-0-AbCdEf",
            "dad-tests-99999999999999-AbCdEf",
        ] {
            assert_eq!(parse_pid(name), None, "{name} must not yield a PID");
        }
    }

    /// The three branches of the decision, with no filesystem in the way. The
    /// two PID-driven ones ignore the age threshold entirely; only the fallback
    /// is decided by it.
    #[test]
    fn ownership_decides_first_and_age_only_where_it_cannot() {
        let max_age = HOUR * 6;
        let fresh = HOUR;
        let stale = HOUR * 9;

        for age in [fresh, stale] {
            assert_eq!(
                classify(Owner::Dead, age, max_age),
                Verdict {
                    reap: true,
                    reason: Reason::DeadPid
                },
                "a dead owner is reaped at any age"
            );
            assert_eq!(
                classify(Owner::Live, age, max_age),
                Verdict {
                    reap: false,
                    reason: Reason::LivePid
                },
                "a live owner is kept at any age"
            );
        }

        assert_eq!(
            classify(Owner::Unknown, stale, max_age),
            Verdict {
                reap: true,
                reason: Reason::UntaggedAge
            }
        );
        assert_eq!(
            classify(Owner::Unknown, fresh, max_age),
            Verdict {
                reap: false,
                reason: Reason::UntaggedAge
            }
        );
    }

    /// Ownership comes from the PID and from nothing else — there is no
    /// recycled branch to reach, so a live PID has exactly one outcome. A
    /// platform that cannot answer liveness is *not* a keep: it falls back to
    /// the age rule exactly as an untagged name does.
    #[test]
    fn a_live_pid_has_exactly_one_outcome_and_an_unanswerable_one_falls_back() {
        let name = "dad-tests-4242-AbCdEf";
        assert_eq!(owner_of(name, &FakeProbe::live()), Owner::Live);
        assert_eq!(owner_of(name, &FakeProbe::dead()), Owner::Dead);
        assert_eq!(owner_of(name, &FakeProbe::unanswerable()), Owner::Unknown);
        assert_eq!(owner_of(".tmpAbCdEf", &FakeProbe::live()), Owner::Unknown);
    }

    /// Issue #461's headline case: 280 roots whose owners were provably gone
    /// were refused because the oldest was under the six-hour default. A dead
    /// PID must now outvote any threshold, however generous.
    #[test]
    fn a_dead_pid_is_reaped_even_when_younger_than_the_threshold() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("dad-tests-4242-AbCdEf");
        std::fs::create_dir(&dir).expect("create");
        std::fs::write(dir.join("payload"), vec![0u8; 2048]).expect("write");

        let found = collect(tmp.path(), &opts(HOUR * 24), &FakeProbe::dead()).expect("collect");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].verdict,
            Verdict {
                reap: true,
                reason: Reason::DeadPid
            }
        );
        assert_eq!(found[0].bytes, 2048);
        assert!(!found[0].size_truncated);
    }

    /// The other half of the fix, driven by the REAL probe: a suite still
    /// running past the threshold used to be eligible to have its own scratch
    /// space deleted out from under it.
    #[cfg(unix)]
    #[test]
    fn a_live_pid_is_kept_even_when_older_than_the_threshold() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp
            .path()
            .join(format!("dad-tests-{}-AbCdEf", std::process::id()));
        std::fs::create_dir(&dir).expect("create");

        // A zero threshold makes every dir "old enough", so only ownership can
        // be keeping this one.
        let found = collect(tmp.path(), &opts(Duration::ZERO), &SystemProbe).expect("collect");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].verdict,
            Verdict {
                reap: false,
                reason: Reason::LivePid
            }
        );
        assert!(dir.exists());
    }

    /// The invariant that replaced the recycling machinery, asserted end to end
    /// through the real deletion path and the real `kill(pid, 0)` probe: a live
    /// owner keeps its root **whatever timestamp the root carries**.
    ///
    /// The two extremes are the two ways the old start-time comparison could be
    /// fooled. A root timestamped in the distant past is what a live long-running
    /// suite looks like once a `--older-than 0` sweep comes past; a root
    /// timestamped in the future is what a single forward `settimeofday` step
    /// makes every *ordinary* root look like relative to a reconstructed start
    /// time, since `btime` moves and inode timestamps do not. Under the old
    /// design either could reach `Recycled` and then be deleted; under this one
    /// no timestamp is read at all outside the age fallback, and the age
    /// fallback is unreachable for a live PID.
    #[cfg(unix)]
    #[test]
    fn a_live_pid_is_kept_whatever_timestamp_the_root_carries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid = std::process::id();
        let ancient = tmp.path().join(format!("dad-tests-{pid}-ancient"));
        let futuristic = tmp.path().join(format!("dad-tests-{pid}-futuristic"));
        for dir in [&ancient, &futuristic] {
            std::fs::create_dir(dir).expect("create");
            std::fs::write(dir.join("payload"), vec![0u8; 512]).expect("write");
        }
        // Decades before this process could possibly have started…
        set_mtime(&ancient, SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        // …and decades after it, which is where a forward clock step used to
        // strand a perfectly ordinary root.
        set_mtime(
            &futuristic,
            SystemTime::now() + Duration::from_secs(50 * 365 * 24 * 3600),
        );

        let applied = sweep(
            tmp.path(),
            &Options {
                max_age: Duration::ZERO,
                apply: true,
                include_untagged: false,
            },
            &SystemProbe,
        )
        .expect("apply");

        assert_eq!(applied.removed, 0, "{}", applied.report);
        assert!(ancient.exists(), "a live owner's root must survive any age");
        assert!(
            futuristic.exists(),
            "a live owner's root must survive a future timestamp too"
        );
        assert!(
            !applied.report.contains("reap:"),
            "nothing may be listed for reaping:\n{}",
            applied.report
        );
        assert!(applied.report.contains("live-pid"), "{}", applied.report);
        // And the mirror image: the timestamp cannot save a dead owner either.
        let dead_tmp = tempfile::tempdir().expect("tempdir");
        let dead = dead_tmp.path().join("dad-tests-4242-futuristic");
        std::fs::create_dir(&dead).expect("create");
        set_mtime(
            &dead,
            SystemTime::now() + Duration::from_secs(365 * 24 * 3600),
        );
        let found =
            collect(dead_tmp.path(), &opts(HOUR * 24), &FakeProbe::dead()).expect("collect");
        assert_eq!(
            found[0].verdict,
            Verdict {
                reap: true,
                reason: Reason::DeadPid
            },
            "a dead PID is reaped whatever the root's timestamp"
        );
    }

    /// Names carrying no usable PID — the pre-fix lock dirs, untagged `.tmp*`
    /// dirs, and malformed roots — keep the original age behaviour in both
    /// directions.
    #[test]
    fn names_without_a_usable_pid_fall_back_to_the_age_rule() {
        for name in [
            "dot-agent-deck-test-lock-AbCdEf",
            "dad-tests-notapid-AbCdEf",
            "dad-tests--AbCdEf",
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::fs::create_dir(tmp.path().join(name)).expect("create");

            let stale =
                collect(tmp.path(), &opts(Duration::ZERO), &FakeProbe::dead()).expect("collect");
            assert_eq!(
                stale[0].verdict,
                Verdict {
                    reap: true,
                    reason: Reason::UntaggedAge
                },
                "{name} past the threshold"
            );

            let fresh = collect(tmp.path(), &opts(HOUR), &FakeProbe::dead()).expect("collect");
            assert_eq!(
                fresh[0].verdict,
                Verdict {
                    reap: false,
                    reason: Reason::UntaggedAge
                },
                "{name} under the threshold"
            );
        }
    }

    /// The invariant that matters most now that `collect` returns kept
    /// candidates too: only the `reap` half is ever handed to `remove_dir_all`.
    /// A test that stops at `collect` cannot see this — it has to drive the
    /// real apply path.
    #[test]
    fn apply_removes_only_the_reap_slice() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let doomed = tmp.path().join("dad-tests-111-AbCdEf");
        let spared = tmp.path().join("dad-tests-222-GhIjKl");
        for dir in [&doomed, &spared] {
            std::fs::create_dir(dir).expect("create");
            std::fs::write(dir.join("payload"), vec![0u8; 1024]).expect("write");
        }
        let probe = DeadPids(vec![111]);

        let dry = sweep(tmp.path(), &opts(Duration::ZERO), &probe).expect("dry run");
        assert_eq!(dry.removed, 0, "a dry run must delete nothing");
        assert!(doomed.exists() && spared.exists());
        assert!(dry.report.contains("dry run"), "{}", dry.report);

        let applied = sweep(
            tmp.path(),
            &Options {
                max_age: Duration::ZERO,
                apply: true,
                include_untagged: false,
            },
            &probe,
        )
        .expect("apply");

        assert!(!doomed.exists(), "the dead owner's root must be removed");
        assert!(
            spared.exists(),
            "the live owner's root must survive --apply, even at a zero threshold"
        );
        assert_eq!(applied.removed, 1);
        assert_eq!(applied.freed, 1024);
        assert!(!applied.freed_truncated);
        assert!(applied.failures.is_empty(), "{:?}", applied.failures);
        assert!(applied.report.contains("live-pid"), "{}", applied.report);
        assert!(!applied.report.contains("dry run"), "{}", applied.report);
    }

    /// Issue #461: the old report stated an age fact ("no owned dirs older than
    /// 6h") while the real question was ownership. Every decision must now name
    /// its reason, kept dirs included.
    #[test]
    fn the_report_attributes_every_decision_to_a_reason() {
        let reap = vec![
            candidate(
                "dad-tests-101-AbCdEf",
                2048,
                1,
                Verdict {
                    reap: true,
                    reason: Reason::DeadPid,
                },
            ),
            candidate(
                ".tmpAbCdEf",
                1024,
                9,
                Verdict {
                    reap: true,
                    reason: Reason::UntaggedAge,
                },
            ),
        ];
        let keep = vec![
            candidate(
                "dad-tests-202-AbCdEf",
                4096,
                12,
                Verdict {
                    reap: false,
                    reason: Reason::LivePid,
                },
            ),
            candidate(
                "dot-agent-deck-test-lock-GhIjKl",
                512,
                1,
                Verdict {
                    reap: false,
                    reason: Reason::UntaggedAge,
                },
            ),
        ];

        let text = report(Path::new("/tmp"), &reap, &keep, HOUR * 6);
        for needle in [
            "dead-pid",
            "untagged",
            "live-pid",
            "owning process is gone",
            "owning process is still running",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        assert!(text.contains("reap: 2 dir(s)"), "{text}");
        assert!(text.contains("keep: 2 dir(s)"), "{text}");
        assert!(!text.contains('≥'), "nothing was truncated:\n{text}");
        assert!(
            !text.contains("recycled"),
            "the recycled reason is gone:\n{text}"
        );
    }

    /// Nothing eligible is no longer reported as an age fact: the summary says
    /// which ownership category held each surviving dir back.
    #[test]
    fn an_empty_reap_set_still_says_why_the_survivors_were_kept() {
        let keep = vec![candidate(
            "dad-tests-202-AbCdEf",
            4096,
            12,
            Verdict {
                reap: false,
                reason: Reason::LivePid,
            },
        )];
        let text = report(Path::new("/tmp"), &[], &keep, HOUR * 6);
        assert!(text.contains("nothing to reap in /tmp"), "{text}");
        assert!(text.contains("live-pid"), "{text}");
        assert!(!text.contains("older than 6h"), "{text}");
    }

    /// A leaking machine accumulates hundreds of roots; the per-directory list
    /// is capped so the summary stays visible, and the cap announces itself
    /// rather than silently truncating.
    #[test]
    fn a_long_reap_list_is_truncated_but_fully_counted() {
        let reap: Vec<Candidate> = (0..MAX_LISTED + 5)
            .map(|i| {
                candidate(
                    &format!("dad-tests-{i}-AbCdEf"),
                    1024,
                    1,
                    Verdict {
                        reap: true,
                        reason: Reason::DeadPid,
                    },
                )
            })
            .collect();
        let text = report(Path::new("/tmp"), &reap, &[], HOUR * 6);
        assert_eq!(
            text.lines().filter(|l| l.contains("dad-tests-")).count(),
            MAX_LISTED
        );
        assert!(text.contains("… and 5 more"), "{text}");
        assert!(text.contains("dead-pid"), "{text}");
        assert!(
            text.contains(&format!("{} dir(s)", MAX_LISTED + 5)),
            "the summary must count every candidate, not just the listed ones:\n{text}"
        );
    }

    /// `/tmp` is mode 1777, so the suffix of a `dad-tests-<dead-pid>-*` name is
    /// attacker-controlled text that the default dry run prints straight to a
    /// terminal. It must reach the terminal escaped — an OSC 52 sequence in a
    /// directory name would otherwise rewrite the reader's clipboard.
    #[test]
    fn hostile_path_names_are_escaped_before_printing() {
        let reap = vec![candidate(
            "dad-tests-101-\u{1b}]52;c;aGk=\u{7}\nreap: 999 dir(s)",
            2048,
            1,
            Verdict {
                reap: true,
                reason: Reason::DeadPid,
            },
        )];
        let text = report(Path::new("/tmp"), &reap, &[], HOUR * 6);
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "control characters reached the terminal:\n{text:?}"
        );
        assert!(text.contains("\\u{1b}]52"), "{text:?}");
        assert!(
            text.lines().filter(|l| l.contains("dad-tests-101")).count() == 1,
            "an embedded newline forged a second line:\n{text:?}"
        );
    }

    /// The size walk now runs on every owned root, kept ones included, so an
    /// enormous or planted tree must not be walked to the end. The size becomes
    /// a lower bound and says so; the verdict is unaffected either way.
    #[test]
    fn an_oversized_tree_stops_walking_and_reports_a_lower_bound() {
        let tmp = tempfile::tempdir().expect("tempdir");
        for i in 0..6 {
            std::fs::write(tmp.path().join(format!("f{i}")), vec![0u8; 100]).expect("write");
        }

        let full = dir_size_bounded(tmp.path(), 100, 8);
        assert_eq!(full.bytes, 600);
        assert!(!full.truncated);

        let capped = dir_size_bounded(tmp.path(), 3, 8);
        assert!(capped.truncated, "the entry budget must stop the walk");
        assert!(capped.bytes < 600, "a truncated walk is a lower bound");

        // Depth is bounded independently of entry count.
        let deep = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&deep).expect("create nested");
        std::fs::write(deep.join("buried"), vec![0u8; 4096]).expect("write");
        let shallow = dir_size_bounded(tmp.path(), 100, 1);
        assert!(shallow.truncated, "the depth budget must stop the descent");
        assert_eq!(shallow.bytes, 600, "nothing below the depth cap is counted");

        // Presentation only: the same tree gets the same verdict either way.
        assert_eq!(
            owner_of("dad-tests-4242-AbCdEf", &FakeProbe::dead()),
            Owner::Dead
        );
    }

    /// Sparse files make apparent size attacker-controlled at zero cost, and
    /// `u64` addition panics on overflow in a debug build — which is how a
    /// plain `cargo xtask clean-e2e-tmp` was aborted with `attempt to add with
    /// overflow` by three planted files, before it could clean anything.
    ///
    /// Every accumulation on the path from one file's length to the printed
    /// total must saturate instead: the per-tree walk, the per-reason group
    /// total, the reap/keep totals, and the `freed` counter after `--apply`.
    #[test]
    fn exabyte_sparse_files_saturate_instead_of_aborting_the_sweep() {
        const HUGE: u64 = 8_000_000_000_000_000_000; // 3 of these overflow u64

        let tmp = tempfile::tempdir().expect("tempdir");
        // The auditor's exact shape: a readable, dead-owner root full of sparse
        // files, reachable by the default dry run.
        for root in ["dad-tests-2147483647-AbCdEf", "dad-tests-2147483646-GhIjKl"] {
            let dir = tmp.path().join(root);
            std::fs::create_dir(&dir).expect("create");
            for i in 0..3 {
                sparse_file(&dir.join(format!("sparse{i}")), HUGE);
            }
        }

        // One tree on its own already overflows.
        let one = dir_size_bounded(&tmp.path().join("dad-tests-2147483647-AbCdEf"), 100, 8);
        assert_eq!(one.bytes, u64::MAX, "the walk must saturate, not wrap");
        assert!(!one.truncated, "three entries are well inside the budget");

        // And so do the group totals and the freed counter across two trees.
        let applied = sweep(
            tmp.path(),
            &Options {
                max_age: Duration::ZERO,
                apply: true,
                include_untagged: false,
            },
            &FakeProbe::dead(),
        )
        .expect("apply");
        assert_eq!(applied.removed, 2, "{}", applied.report);
        assert_eq!(applied.freed, u64::MAX);
        assert!(applied.failures.is_empty(), "{:?}", applied.failures);
        assert!(applied.report.contains("dead-pid"), "{}", applied.report);
    }

    /// The same overflow reached through the pure reporting path, where the
    /// group and grand totals are summed independently of the walk.
    #[test]
    fn group_and_grand_totals_saturate() {
        let reap = vec![
            candidate(
                "dad-tests-101-AbCdEf",
                u64::MAX,
                1,
                Verdict {
                    reap: true,
                    reason: Reason::DeadPid,
                },
            ),
            candidate(
                "dad-tests-102-GhIjKl",
                u64::MAX,
                1,
                Verdict {
                    reap: true,
                    reason: Reason::DeadPid,
                },
            ),
        ];
        let text = report(Path::new("/tmp"), &reap, &[], HOUR * 6);
        assert!(text.contains("reap: 2 dir(s)"), "{text}");
        assert!(text.contains("dead-pid"), "{text}");
    }

    /// `--older-than` takes hours from the command line and multiplies by 3600.
    /// A value near `u64::MAX` used to panic the command outright in a debug
    /// build rather than parse into an absurdly distant threshold.
    #[test]
    fn an_enormous_older_than_saturates_rather_than_panicking() {
        let args = vec!["--older-than".to_string(), u64::MAX.to_string()];
        let parsed = parse_args(&args).expect("parse").expect("options");
        assert_eq!(parsed.max_age, Duration::from_secs(u64::MAX));
    }

    /// The sizing walk resolves every queued directory by path, so a symlink
    /// swapped into that path after it was observed to be a directory would be
    /// followed by `read_dir`. This drives that state directly — the walk is
    /// handed a path that is a symlink by the time it opens it — and the
    /// re-check must refuse to descend, leaving the size at zero rather than
    /// counting a tree outside the candidate.
    ///
    /// It is a narrowing, not a fix: a swap undone again inside the window
    /// still slips through, which is why the guarantee is documented as bounded
    /// rather than absolute.
    #[cfg(unix)]
    #[test]
    fn a_directory_replaced_by_a_symlink_is_not_descended() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).expect("create outside");
        std::fs::write(outside.join("payload"), vec![0u8; 4096]).expect("write");

        let root = tmp.path().join("root");
        std::fs::create_dir(&root).expect("create root");
        std::fs::write(root.join("own"), vec![0u8; 100]).expect("write");
        assert_eq!(dir_size_bounded(&root, 100, 8).bytes, 100);

        std::fs::remove_dir_all(&root).expect("remove root");
        std::os::unix::fs::symlink(&outside, &root).expect("symlink");
        let size = dir_size_bounded(&root, 100, 8);
        assert_eq!(
            size.bytes, 0,
            "the walk descended a path that had become a symlink"
        );
        assert!(!size.truncated);
        assert!(
            outside.join("payload").exists(),
            "nothing outside is touched"
        );
    }

    /// A truncated size is marked in the per-directory line, in the group
    /// totals, and explained once at the end.
    #[test]
    fn a_truncated_size_is_marked_as_a_lower_bound() {
        let mut reap = vec![candidate(
            "dad-tests-101-AbCdEf",
            5 * 1024 * 1024,
            1,
            Verdict {
                reap: true,
                reason: Reason::DeadPid,
            },
        )];
        reap[0].size_truncated = true;
        let text = report(Path::new("/tmp"), &reap, &[], HOUR * 6);
        assert!(text.contains("≥ 5.0 MB"), "{text}");
        assert!(text.contains("reap: 1 dir(s), ≥ 5.0 MB"), "{text}");
        assert!(text.contains("lower bounds"), "{text}");
    }
}
