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
//! So the PID decides wherever it can, and `--older-than` only filters the cases
//! it cannot settle:
//!
//! - **dead PID** → reap, at any age. `--older-than` does not suppress it.
//! - **live PID** → keep, at any age.
//! - **live PID provably started after the root existed** → the number was
//!   recycled and says nothing about this root, so the age rule decides.
//!   Without this branch a recycled PID would pin a dead root forever.
//! - **no usable PID** — an untagged `.tmp*` dir, a pre-fix lock dir, a
//!   malformed name, or a platform with no `kill(2)` — the age rule decides.
//!
//! Liveness is `kill(pid, 0)`, in which `EPERM` counts as **alive**: the process
//! exists, it merely is not ours, and reading that as dead would delete a live
//! run's root.
//!
//! Recycling has to be *proven*, and the proof is deliberately expensive to
//! obtain: field 22 of `/proc/<pid>/stat` converted to wall clock (see
//! [`process_start_time`]), compared against the root's own creation time with a
//! [`RECYCLE_MARGIN`] of slack. Every way that proof can fail — no `/proc`, an
//! unparseable field, no `btime`, a bad `_SC_CLK_TCK`, a filesystem that reports
//! no timestamp, a non-Linux target — resolves to **keep**, not to the age rule.
//! The tradeoff is accepted knowingly: a genuinely recycled PID we cannot prove
//! recycled pins its root forever, and leaking a root is strictly better than
//! deleting a live run's working directory.
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

/// How much later than a root's own timestamp a process must have started
/// before we are willing to call its PID recycled.
///
/// The two timestamps come from unrelated clocks: the process start time is
/// derived from the kernel's boot time plus a tick counter, while the root's is
/// whatever `$TMPDIR`'s filesystem recorded — which on a coarse-granularity, a
/// network, or a clock-skewed filesystem is not safely orderable against the
/// first at second resolution. Five minutes is far longer than any such skew and
/// far shorter than the gap in a real recycling (a PID space wraps after tens of
/// thousands of spawns), so the margin costs nothing in detection and removes a
/// whole class of false positives — each of which would delete a live run's
/// scratch space.
const RECYCLE_MARGIN: Duration = Duration::from_secs(5 * 60);

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
    /// A live PID that could have created this root — including every case
    /// where we cannot *prove* it could not.
    Live,
    /// A live PID whose process is proven to have started well after the root
    /// already existed, so it cannot be the creator: the number was recycled.
    Recycled,
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
    /// The PID was recycled, so the age rule decided.
    RecycledAge,
    /// There was no usable PID, so the age rule decided.
    UntaggedAge,
}

impl Reason {
    /// Fixed order, so the summary reads the same way on every run.
    const ALL: [Reason; 4] = [
        Reason::DeadPid,
        Reason::LivePid,
        Reason::RecycledAge,
        Reason::UntaggedAge,
    ];

    fn label(self) -> &'static str {
        match self {
            Reason::DeadPid => "dead-pid",
            Reason::LivePid => "live-pid",
            Reason::RecycledAge => "recycled",
            Reason::UntaggedAge => "untagged",
        }
    }

    /// One-line justification for the summary. The age-based reasons need the
    /// threshold and which side of it the dirs fell on; the PID-based ones are
    /// unconditional and say so.
    fn note(self, reap: bool, max_age: Duration) -> String {
        let age = human_duration(max_age);
        let side = if reap { "older" } else { "younger" };
        match self {
            Reason::DeadPid => "owning process is gone — reaped at any age".to_string(),
            Reason::LivePid => "owning process is still running — never reaped".to_string(),
            Reason::RecycledAge => {
                format!("PID reused by a newer process; {side} than {age}")
            }
            Reason::UntaggedAge => format!("no owning PID in the name; {side} than {age}"),
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

/// The two process facts the ownership decision needs, behind a trait so the
/// classification matrix can be driven from a table rather than from real PIDs.
///
/// The dead-PID test used to spawn a child, `wait()` it, and then probe the
/// number — but the kernel may reassign a PID the moment it is reaped, so under
/// PID churn that test observed an unrelated live process (issue #461 review).
/// Injecting the probes removes the race and, more importantly, makes every
/// branch of [`owner_of`] reachable without arranging a real process to match.
trait ProcessProbe {
    /// `Some(true)` alive, `Some(false)` dead, `None` where this platform
    /// cannot tell.
    fn is_alive(&self, pid: i32) -> Option<bool>;

    /// When the process holding `pid` started, or `None` when that cannot be
    /// established — which always resolves to keeping the root.
    fn start_time(&self, pid: i32) -> Option<SystemTime>;
}

/// The real probes: `kill(pid, 0)` for liveness, `/proc/<pid>/stat` for start.
struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn is_alive(&self, pid: i32) -> Option<bool> {
        pid_is_alive(pid)
    }

    fn start_time(&self, pid: i32) -> Option<SystemTime> {
        process_start_time(pid)
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
                opts.max_age = Duration::from_secs(hours * 3600);
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
    println!("names, and PIDs proven to have been recycled by a newer process.");
    println!();
    println!("  --older-than <hours>  age threshold for the fallback cases only");
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
                        freed += c.bytes;
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
        let mtime = meta.modified().ok();
        let age = mtime
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        // Creation time (`statx` btime on Linux) is the timestamp the recycling
        // test actually wants: "did this root already exist when that process
        // started?". mtime is the fallback where the filesystem reports no
        // btime, and the fallback direction is safe — mtime is never earlier
        // than creation, so it makes `start > dir_time` harder to satisfy and
        // biases towards keeping the root.
        let dir_time = meta.created().ok().or(mtime);
        // Every owned dir is collected, kept ones included: the report has to
        // be able to say WHY a root survived, which it cannot do for entries
        // that were filtered away before they were ever seen.
        let verdict = classify(owner_of(name, dir_time, probe), age, opts.max_age);
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
        // Strictly safer than the age rule it replaces, which made a suite
        // running past the threshold eligible for its own root.
        Owner::Live => Verdict {
            reap: false,
            reason: Reason::LivePid,
        },
        Owner::Recycled => Verdict {
            reap: age >= max_age,
            reason: Reason::RecycledAge,
        },
        Owner::Unknown => Verdict {
            reap: age >= max_age,
            reason: Reason::UntaggedAge,
        },
    }
}

/// Classify a root from its name plus the best timestamp the filesystem has for
/// it (creation time where available, mtime otherwise).
///
/// The asymmetry is the point: `Owner::Recycled` is the only verdict that can
/// lead to deleting a directory whose PID *is* alive, so it is the only one that
/// demands positive proof. These all resolve to `Owner::Live`, i.e. keep
/// forever:
///
/// - the process start time is unavailable — no `/proc`, an unreadable or
///   unparseable `stat`, no `btime` in `/proc/stat`, a bad `_SC_CLK_TCK`, or a
///   non-Linux target;
/// - the root carries no usable timestamp;
/// - the process started before the root's timestamp, or after it by less than
///   [`RECYCLE_MARGIN`].
fn owner_of(name: &str, dir_time: Option<SystemTime>, probe: &dyn ProcessProbe) -> Owner {
    let Some(pid) = parse_pid(name) else {
        return Owner::Unknown;
    };
    match probe.is_alive(pid) {
        Some(false) => Owner::Dead,
        // The platform cannot answer, so fall back to age exactly as an
        // untagged name would.
        None => Owner::Unknown,
        Some(true) => {
            let cutoff = dir_time.and_then(|d| d.checked_add(RECYCLE_MARGIN));
            match (probe.start_time(pid), cutoff) {
                (Some(started), Some(cutoff)) if started > cutoff => Owner::Recycled,
                _ => Owner::Live,
            }
        }
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

/// When the process holding `pid` started, as an absolute wall-clock instant.
///
/// Field 22 (`starttime`) of `/proc/<pid>/stat` is the authoritative answer: the
/// kernel stamps it once, when the task is created, and never rewrites it. It
/// counts clock ticks since boot, so it is converted with `/proc/stat`'s `btime`
/// (the boot instant, in wall-clock seconds) and `sysconf(_SC_CLK_TCK)` for the
/// tick rate — never a hardcoded 100, which is only the usual `CONFIG_HZ`.
///
/// `/proc/uptime` would serve equally well, but `btime` is preferred because it
/// is already an absolute instant: the conversion needs no second "now" reading,
/// so there is no window between the two clock reads for either to drift.
///
/// This replaces the `/proc/<pid>` **directory mtime**, which is emphatically
/// NOT a start time. Linux instantiates the per-PID procfs inode lazily —
/// `proc_pid_make_inode()` initialises its timestamps at instantiation and
/// `pid_getattr()` leaves them alone rather than substituting the task's start
/// time — so that mtime is a proc-dentry *lookup* time and moves forward again
/// whenever the dentry is evicted and re-looked-up, which is most likely under
/// exactly the memory pressure this tool exists to relieve. A live owner whose
/// root predated the first `/proc/<pid>` lookup was therefore reported
/// "recycled", and under `--apply` its live working directory was deleted
/// (issue #461 review).
///
/// Every failure path returns `None`, which [`owner_of`] reads as "keep".
#[cfg(target_os = "linux")]
fn process_start_time(pid: i32) -> Option<SystemTime> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = starttime_ticks(&stat)?;
    let hz = clock_ticks_per_second()?;
    let nanos = u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(hz)).ok()?;
    boot_time()?.checked_add(Duration::from_nanos(nanos))
}

/// Field 22 out of one `/proc/<pid>/stat` line, in clock ticks since boot.
///
/// Field 2 is `comm`, wrapped in parentheses and **not** escaped: a process
/// named `foo bar) baz` puts both spaces and a `)` inside it, so a
/// `split_whitespace()` over the whole line silently reads some other number.
/// `comm` is the only parenthesised field and every field after it is numeric,
/// so splitting at the **last** `)` is unambiguous. Counting resumes there:
/// field 3 is index 0 of the remainder, so field 22 is index 19.
#[cfg(target_os = "linux")]
fn starttime_ticks(stat: &str) -> Option<u64> {
    let after_comm = stat.get(stat.rfind(')')? + 1..)?;
    after_comm.split_whitespace().nth(19)?.parse().ok()
}

/// `sysconf(_SC_CLK_TCK)`, the unit field 22 is counted in.
#[cfg(target_os = "linux")]
fn clock_ticks_per_second() -> Option<u64> {
    // SAFETY: `sysconf` reads a static system parameter, takes no pointer and
    // writes no memory we own. It returns -1 for an unsupported name, which the
    // `try_from` + `> 0` filter rejects into `None` (keep the root).
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(hz).ok().filter(|hz| *hz > 0)
}

/// The wall-clock instant the machine booted, from `/proc/stat`'s `btime` line.
#[cfg(target_os = "linux")]
fn boot_time() -> Option<SystemTime> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let secs: u64 = stat
        .lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

#[cfg(not(target_os = "linux"))]
fn process_start_time(_pid: i32) -> Option<SystemTime> {
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
        let bytes = matching.iter().map(|c| c.bytes).sum();
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
        cands.iter().map(|c| c.bytes).sum(),
        cands.iter().any(|c| c.size_truncated),
    )
}

/// Apparent size of one tree, plus whether the walk gave up before finishing.
struct DirSize {
    bytes: u64,
    truncated: bool,
}

/// Recursive apparent size, bounded by [`MAX_SIZE_WALK_ENTRIES`] and
/// [`MAX_SIZE_WALK_DEPTH`]. Never follows symlinks, so a link out of the tree
/// contributes its own size and nothing more.
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
                bytes += meta.len();
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
    /// number to someone else before the probe runs — nor "started three hours
    /// after that directory" at all.
    struct FakeProbe {
        alive: Option<bool>,
        start: Option<SystemTime>,
    }

    impl FakeProbe {
        fn dead() -> Self {
            Self {
                alive: Some(false),
                start: None,
            }
        }
        fn live_started(start: SystemTime) -> Self {
            Self {
                alive: Some(true),
                start: Some(start),
            }
        }
        fn live_with_unknown_start() -> Self {
            Self {
                alive: Some(true),
                start: None,
            }
        }
        fn unanswerable() -> Self {
            Self {
                alive: None,
                start: None,
            }
        }
    }

    impl ProcessProbe for FakeProbe {
        fn is_alive(&self, _pid: i32) -> Option<bool> {
            self.alive
        }
        fn start_time(&self, _pid: i32) -> Option<SystemTime> {
            self.start
        }
    }

    /// A probe that answers per PID, for tests that need a reapable root and a
    /// kept root side by side in one directory.
    struct DeadPids(Vec<i32>);

    impl ProcessProbe for DeadPids {
        fn is_alive(&self, pid: i32) -> Option<bool> {
            Some(!self.0.contains(&pid))
        }
        fn start_time(&self, _pid: i32) -> Option<SystemTime> {
            None // unknown start ⇒ a live PID owns its root
        }
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

    /// The four branches of the decision, with no filesystem in the way. The
    /// two PID-driven ones ignore the age threshold entirely; the two fallback
    /// ones are decided by it.
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

        for (owner, reason) in [
            (Owner::Recycled, Reason::RecycledAge),
            (Owner::Unknown, Reason::UntaggedAge),
        ] {
            assert_eq!(
                classify(owner, stale, max_age),
                Verdict { reap: true, reason }
            );
            assert_eq!(
                classify(owner, fresh, max_age),
                Verdict {
                    reap: false,
                    reason
                }
            );
        }
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

    /// The other half of the fix, driven by the REAL probes: a suite still
    /// running past the threshold used to be eligible to have its own scratch
    /// space deleted out from under it. This is the reviewer's repro of the
    /// procfs-mtime blocker in test form — this process is genuinely alive and
    /// genuinely started before the directory, so nothing but `Live` is correct
    /// no matter when `/proc/<pid>` was first looked up.
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

    /// The full ownership matrix, driven through injected probes so every
    /// branch is reachable without arranging a real process to match.
    ///
    /// The middle two rows are the blocker from the #461 review: the root
    /// predates the process by *less* than the margin (clock/filesystem skew,
    /// not recycling) and must be kept, while a process that started hours
    /// later is genuinely a different one.
    #[test]
    fn recycling_is_only_claimed_when_the_start_time_proves_it() {
        let name = "dad-tests-4242-AbCdEf";
        let dir_time = SystemTime::UNIX_EPOCH + HOUR * 1_000;

        assert_eq!(
            owner_of(
                name,
                Some(dir_time),
                &FakeProbe::live_started(dir_time - HOUR)
            ),
            Owner::Live,
            "a process older than the root is its owner"
        );
        assert_eq!(
            owner_of(
                name,
                Some(dir_time),
                &FakeProbe::live_started(dir_time + RECYCLE_MARGIN - Duration::from_secs(1)),
            ),
            Owner::Live,
            "inside the margin is skew, not recycling"
        );
        assert_eq!(
            owner_of(
                name,
                Some(dir_time),
                &FakeProbe::live_started(dir_time + RECYCLE_MARGIN + Duration::from_secs(1)),
            ),
            Owner::Recycled,
            "past the margin the process cannot have created the root"
        );
        assert_eq!(
            owner_of(
                name,
                Some(dir_time),
                &FakeProbe::live_started(dir_time + HOUR * 3)
            ),
            Owner::Recycled,
        );
        assert_eq!(
            owner_of(name, Some(dir_time), &FakeProbe::dead()),
            Owner::Dead,
        );

        // The recycled verdict is the only one that can delete a live PID's
        // root, and even then only the age rule actually pulls the trigger.
        let max_age = HOUR * 6;
        assert!(classify(Owner::Recycled, HOUR * 9, max_age).reap);
        assert!(!classify(Owner::Recycled, HOUR, max_age).reap);
    }

    /// Every way the recycling proof can come up short resolves to `Live`, i.e.
    /// keep forever. Leaking a root beats deleting a live run's working
    /// directory, so this list is the whole safety argument for the feature.
    #[test]
    fn every_uncertain_probe_result_keeps_the_root() {
        let name = "dad-tests-4242-AbCdEf";
        let dir_time = SystemTime::UNIX_EPOCH + HOUR * 1_000;

        // No start time at all: unreadable or unparseable `/proc/<pid>/stat`,
        // a missing `btime`, a bad `_SC_CLK_TCK`, or a non-Linux target.
        assert_eq!(
            owner_of(name, Some(dir_time), &FakeProbe::live_with_unknown_start()),
            Owner::Live,
        );
        // The filesystem reported no timestamp for the root.
        assert_eq!(
            owner_of(name, None, &FakeProbe::live_started(dir_time + HOUR * 3)),
            Owner::Live,
        );
        assert_eq!(
            owner_of(name, None, &FakeProbe::live_with_unknown_start()),
            Owner::Live,
        );
        // A platform that cannot answer liveness at all is not a keep — it is
        // the pre-#461 age rule, exactly as an untagged name would be.
        assert_eq!(
            owner_of(name, Some(dir_time), &FakeProbe::unanswerable()),
            Owner::Unknown,
        );
    }

    /// The start time must be the kernel's own record, not a procfs lookup
    /// timestamp. `init` booted before this test binary was ever spawned, and
    /// no amount of dentry churn can change that — but the `/proc/<pid>` mtime
    /// this replaced could report either order depending on which entry was
    /// looked up last.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_start_time_is_the_kernels_record_not_a_procfs_lookup_time() {
        let boot = boot_time().expect("btime in /proc/stat");
        let init = process_start_time(1).expect("start time of pid 1");
        let me = process_start_time(i32::try_from(std::process::id()).expect("pid fits"))
            .expect("start time of this process");

        assert!(init >= boot, "init cannot predate the boot instant");
        assert!(init <= me, "init started before this test process");
        assert!(me <= SystemTime::now(), "this process has already started");
        // A lookup time would be seconds old at most; a start time carries the
        // real distance back to boot.
        assert!(
            init.duration_since(boot).expect("init after boot") < HOUR,
            "pid 1 starts within an hour of boot"
        );
    }

    /// Field 22 is counted from after `comm`, which is unescaped and may hold
    /// both spaces and parentheses — the case a `split_whitespace()` over the
    /// whole line gets silently wrong.
    #[cfg(target_os = "linux")]
    #[test]
    fn field_22_is_parsed_from_after_the_last_closing_paren() {
        let tail = "0 -1 4194304 147 0 0 0 0 0 0 0 20 0 1 0 107253507 10383360 723";
        assert_eq!(
            starttime_ticks(&format!("4177459 (head) R 4177425 4177459 4177425 {tail}")),
            Some(107253507),
        );
        assert_eq!(
            starttime_ticks(&format!(
                "4177459 (evil ) name) R 4177425 4177459 4177425 {tail}"
            )),
            Some(107253507),
            "a comm holding a space and a paren must not shift the field index"
        );
        for malformed in [
            "",
            "4177459 (head",
            "4177459 (head) R 1 2 3",
            "4177459 (head) R 4177425 4177459 4177425 0 -1 x 147 0 0 0 0 0 0 0 20 0 1 0 nope 1 2",
        ] {
            assert_eq!(
                starttime_ticks(malformed),
                None,
                "{malformed:?} must not yield a start time"
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
                "dad-tests-303-AbCdEf",
                512,
                1,
                Verdict {
                    reap: false,
                    reason: Reason::RecycledAge,
                },
            ),
        ];

        let text = report(Path::new("/tmp"), &reap, &keep, HOUR * 6);
        for needle in [
            "dead-pid",
            "untagged",
            "live-pid",
            "recycled",
            "owning process is gone",
            "owning process is still running",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        assert!(text.contains("reap: 2 dir(s)"), "{text}");
        assert!(text.contains("keep: 2 dir(s)"), "{text}");
        assert!(!text.contains('≥'), "nothing was truncated:\n{text}");
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
        let name = "dad-tests-4242-AbCdEf";
        assert_eq!(
            owner_of(name, Some(SystemTime::now()), &FakeProbe::dead()),
            Owner::Dead
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
