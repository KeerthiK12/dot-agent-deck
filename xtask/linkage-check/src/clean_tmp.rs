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
//! - **live PID whose process started after the root's mtime** → the number was
//!   recycled and says nothing about this root, so the age rule decides.
//!   Without this branch a recycled PID would pin a dead root forever.
//! - **no usable PID** — an untagged `.tmp*` dir, a pre-fix lock dir, a
//!   malformed name, or a platform with no `kill(2)` — the age rule decides.
//!
//! Liveness is `kill(pid, 0)`, in which `EPERM` counts as **alive**: the process
//! exists, it merely is not ours, and reading that as dead would delete a live
//! run's root. Start time comes from the `/proc/<pid>` directory's mtime on
//! Linux; where it cannot be determined the live process is assumed to be the
//! owner (kept), never guessed away.
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
    /// A live PID that could have created this root.
    Live,
    /// A live PID whose process started *after* the root's mtime, so it cannot
    /// be the creator — the number was recycled.
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
    age: Duration,
    verdict: Verdict,
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
    let candidates = match collect(&temp_root, &opts) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "xtask clean-e2e-tmp: cannot read {}: {e}",
                temp_root.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let (reap, keep): (Vec<Candidate>, Vec<Candidate>) =
        candidates.into_iter().partition(|c| c.verdict.reap);
    print!("{}", report(&temp_root, &reap, &keep, opts.max_age));

    if reap.is_empty() {
        return ExitCode::SUCCESS;
    }

    if !opts.apply {
        println!();
        println!("dry run — nothing removed. Re-run with --apply to delete.");
        return ExitCode::SUCCESS;
    }

    let mut removed = 0usize;
    let mut freed = 0u64;
    let mut failures = 0usize;
    for c in &reap {
        match std::fs::remove_dir_all(&c.path) {
            Ok(()) => {
                removed += 1;
                freed += c.bytes;
            }
            Err(e) => {
                failures += 1;
                eprintln!("  failed to remove {}: {e}", c.path.display());
            }
        }
    }
    println!("removed {removed} dir(s), freed {}", human_bytes(freed));
    if failures > 0 {
        eprintln!("{failures} dir(s) could not be removed");
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
    println!("names, and PIDs since recycled by a newer process.");
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

fn collect(temp_root: &Path, opts: &Options) -> std::io::Result<Vec<Candidate>> {
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
        // Every owned dir is collected, kept ones included: the report has to
        // be able to say WHY a root survived, which it cannot do for entries
        // that were filtered away before they were ever seen.
        let verdict = classify(owner_of(name, mtime), age, opts.max_age);
        out.push(Candidate {
            bytes: dir_size(&path),
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

/// Classify a root from its name plus its mtime. `dir_mtime` is `None` when the
/// filesystem would not report one, which only costs us the recycled-PID check.
fn owner_of(name: &str, dir_mtime: Option<SystemTime>) -> Owner {
    let Some(pid) = parse_pid(name) else {
        return Owner::Unknown;
    };
    match pid_is_alive(pid) {
        Some(false) => Owner::Dead,
        // The platform cannot answer, so fall back to age exactly as an
        // untagged name would.
        None => Owner::Unknown,
        Some(true) => match (process_start_time(pid), dir_mtime) {
            // The owner cannot have started after its own root was last
            // written, so this PID belongs to a different, newer process.
            // Comparing against mtime rather than creation time is the
            // conservative direction: mtime is never earlier than creation, so
            // this under-reports recycling and never over-reports it.
            (Some(started), Some(dir)) if started > dir => Owner::Recycled,
            // No start time available: assume the live process IS the owner
            // and keep the root. Guessing the other way deletes a live run's
            // scratch space.
            _ => Owner::Live,
        },
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

/// When the process holding `pid` started, used only to spot a recycled PID.
///
/// `/proc/<pid>` is created when the process is, so its mtime is the process's
/// start time — cheaper and less error-prone than reconstructing field 22 of
/// `/proc/<pid>/stat` from the boot time and the clock tick. `None` anywhere
/// else, which makes the caller keep the root rather than guess.
#[cfg(target_os = "linux")]
fn process_start_time(pid: i32) -> Option<SystemTime> {
    std::fs::metadata(format!("/proc/{pid}"))
        .ok()?
        .modified()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn process_start_time(_pid: i32) -> Option<SystemTime> {
    None
}

/// The human-facing report, built as a string so a test can assert that it
/// attributes each decision. Issue #461: the old output said "no owned dirs
/// older than 6h" — an age fact, when the question was ownership.
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
            "  {:>9}  {:<9} {:>4} old  {}",
            human_bytes(c.bytes),
            c.verdict.reason.label(),
            human_duration(c.age),
            c.path.display(),
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
            human_bytes(total_bytes(reap)),
            temp_root.display(),
        );
        write_breakdown(&mut out, reap, true, max_age);
    }

    if !keep.is_empty() {
        let _ = writeln!(
            out,
            "keep: {} dir(s), {}",
            keep.len(),
            human_bytes(total_bytes(keep)),
        );
        write_breakdown(&mut out, keep, false, max_age);
    }
    out
}

/// Counts and sizes per reason — the part that stays readable when there are
/// 280 candidates.
fn write_breakdown(out: &mut String, cands: &[Candidate], reap: bool, max_age: Duration) {
    for reason in Reason::ALL {
        let matching = cands.iter().filter(|c| c.verdict.reason == reason);
        let (count, bytes) = matching.fold((0usize, 0u64), |(n, b), c| (n + 1, b + c.bytes));
        if count == 0 {
            continue;
        }
        let _ = writeln!(
            out,
            "  {:<9} {:>4} dir(s)  {:>9}  {}",
            reason.label(),
            count,
            human_bytes(bytes),
            reason.note(reap, max_age),
        );
    }
}

fn total_bytes(cands: &[Candidate]) -> u64 {
    cands.iter().map(|c| c.bytes).sum()
}

/// Recursive apparent size. Never follows symlinks, so a link out of the tree
/// contributes its own size and nothing more.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
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
            age: HOUR * age_hours,
            verdict,
        }
    }

    /// A PID we know is dead because we waited for it ourselves. Reusing a
    /// reaped child beats hardcoding a "probably unused" number, which is
    /// flaky by construction.
    #[cfg(unix)]
    fn reaped_child_pid() -> i32 {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn /bin/sh");
        let pid = i32::try_from(child.id()).expect("pid fits in pid_t");
        child.wait().expect("wait for child");
        pid
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
        let found = collect(tmp.path(), &opts(Duration::ZERO)).expect("collect");
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
    #[cfg(unix)]
    #[test]
    fn a_dead_pid_is_reaped_even_when_younger_than_the_threshold() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp
            .path()
            .join(format!("dad-tests-{}-AbCdEf", reaped_child_pid()));
        std::fs::create_dir(&dir).expect("create");
        std::fs::write(dir.join("payload"), vec![0u8; 2048]).expect("write");

        let found = collect(tmp.path(), &opts(HOUR * 24)).expect("collect");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].verdict,
            Verdict {
                reap: true,
                reason: Reason::DeadPid
            }
        );
        assert_eq!(found[0].bytes, 2048);
    }

    /// The other half of the fix: a suite still running past the threshold used
    /// to be eligible to have its own scratch space deleted out from under it.
    /// This process is trivially alive and trivially started before the dir.
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
        let found = collect(tmp.path(), &opts(Duration::ZERO)).expect("collect");
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

            let stale = collect(tmp.path(), &opts(Duration::ZERO)).expect("collect");
            assert_eq!(
                stale[0].verdict,
                Verdict {
                    reap: true,
                    reason: Reason::UntaggedAge
                },
                "{name} past the threshold"
            );

            let fresh = collect(tmp.path(), &opts(HOUR)).expect("collect");
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

    /// A live PID that started after the root was last written cannot be its
    /// creator, so the number has been recycled and decides nothing — without
    /// this branch one recycled PID would pin a dead root forever. Needs
    /// `/proc`, hence Linux-only.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_recycled_pid_falls_back_to_the_age_rule() {
        let name = format!("dad-tests-{}-AbCdEf", std::process::id());
        // This process cannot have created a directory last written in 1970.
        assert_eq!(
            owner_of(&name, Some(SystemTime::UNIX_EPOCH)),
            Owner::Recycled
        );
        // ...but it could have created one written just now.
        assert_eq!(owner_of(&name, Some(SystemTime::now())), Owner::Live);
        // No mtime to compare against means no recycling claim, and a live
        // owner is kept.
        assert_eq!(owner_of(&name, None), Owner::Live);

        let max_age = HOUR * 6;
        assert!(classify(Owner::Recycled, HOUR * 9, max_age).reap);
        assert!(!classify(Owner::Recycled, HOUR, max_age).reap);
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
}
