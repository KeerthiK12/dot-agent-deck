# E2E temp directories

How the test harness allocates scratch space, why it used to leak, and what to run when a machine has accumulated leftovers. Background: [issue #322](https://github.com/vfarcic/dot-agent-deck/issues/322).

## The one-root rule

Every temp directory the harness creates nests under a single per-process root named `dad-tests-<pid>-<random>`, allocated in the system temp dir on first use. `race_safe_tempdir()` and the daemon lock dir both go through it, so there is exactly one place to look and exactly one thing to clean up.

The root is removed by an `atexit(3)` hook when the test process exits normally. The hook retries a few times before giving up, because a daemon or agent the test spawned can outlive the test body for a moment and keep writing into the tree, making the first sweep lose a race. If it still cannot remove the root it says so on stderr rather than failing silently.

A process that is **SIGKILLed** never reaches the hook at all and leaves its root behind. That is the one remaining leak path and it is what the reaper below exists for. Measured on a full `cargo test-e2e` run of 3,347 tests: **16 roots totalling ~360 MB**, down from 46 before the retry was added. Running the same tests in isolation leaks nothing, so this is a symptom of parallel load, related to the contention described in [#351](https://github.com/vfarcic/dot-agent-deck/issues/351).

Two properties matter and are easy to break if you touch this code:

- **The root must be created through the single choke point.** `harness_temp_root()` is where the pre-flight space check runs and where 0o700 hardening is applied. A temp dir allocated with a bare `tempfile::tempdir()` escapes both.
- **The name must stay distinctive.** The `tempfile` crate's *default* prefix is `.tmp`, so `/tmp/.tmp*` belongs to every Rust program on the machine. The reaper can only safely delete names this repo owns, which is why the root is not simply another `.tmp*` dir.

## Why an `atexit` hook rather than `Drop`

The lock dir this replaced was held in a `static OnceLock<TempDir>`. **Rust does not run destructors for statics at process exit**, so its `TempDir::drop` never fired. Because nextest runs one process per *test*, that leaked one directory per test — on a fully green run. Measured before the fix: 13 directories from 56 passing tests, and 6,667 accumulated on one dev machine over eight days.

If you ever need process-lifetime scratch state, do not reach for a `static TempDir`. Put it under the harness root and let the exit hook take it.

## Reaping leftovers

```bash
cargo xtask clean-e2e-tmp                      # dry run — always start here
cargo xtask clean-e2e-tmp --apply              # actually delete
cargo xtask clean-e2e-tmp --older-than 1 --apply
```

Dry-run is the default and `--apply` is required to delete anything. By default it only considers directories this repo owns:

| Prefix | Reaped by default | Why |
|---|---|---|
| `dad-tests-*` | yes | The current harness root. |
| `dot-agent-deck-test-lock-*` | yes | Pre-fix lock dirs, still present in bulk on older machines. |
| `.tmp*` | **no** — needs `--include-untagged` | The `tempfile` crate's default prefix, shared with every Rust program on the machine. |

Only pass `--include-untagged` when no other Rust build or tool is running; it can otherwise delete a live temp dir belonging to something else.

### Ownership decides, age is only the fallback

The `<pid>` in `dad-tests-<pid>-<random>` is the process that **created** the root, so it answers "is the creator still running?" — a much better proxy for "is anyone still using this?" than age, but not the same question (see the orphan window below). Since [issue #461](https://github.com/vfarcic/dot-agent-deck/issues/461) the reaper reads it back, and the decision is these branches:

| The root's PID | Decision |
|---|---|
| No live process holds it | **Reap, once the root is ≥ 10 minutes old.** `--older-than` does not hold it back; the 10-minute floor is separate and much shorter. |
| Held by a live process | **Keep, at any age.** No timestamp is consulted. |
| Held by a live process, with `--ignore-liveness` | Decided by age — the opt-in escape for a PID reused across a reboot. |
| No usable PID — an untagged `.tmp*` dir, a lock dir, a malformed name, or a host with no `kill(2)` at all | Decided by age. |

Liveness is `kill(pid, 0)`, in which `EPERM` counts as **alive** — the process exists, it merely belongs to another user — and only `ESRCH` means dead. That call exists on every Unix, so the last row's "no `kill(2)`" case means a genuinely non-Unix host, not merely a non-Linux one; macOS and the BSDs establish liveness exactly as Linux does. One thing it cannot tell you: `kill(2)` answers about the *caller's* PID namespace, so a host-side reaper probing names written inside a container that bind-mounts `/tmp` would be answering about the wrong namespace. No workflow here does that — the tooling is `devbox`, same namespace — but note the failure is a *wrong* answer, not an unavailable one, so no fallback triggers.

Age alone once refused 280 roots totalling **6.2 GB**, every one with a dead owner, because the oldest was 4h09m and the default threshold is 6h — on a 14 GB tmpfs with 5 MiB of swap left and an e2e compile about to start. It also made the opposite mistake: a suite still running past the threshold was eligible to have its own live root deleted out from under it. Both directions matter, and the second is why the live row is unconditional.

#### The 10-minute floor on dead owners, and why it is not optional

Owner death is not the same as the tree being unreferenced, and this codebase widens that gap deliberately. The name carries the *test process's* PID, but the processes that test spawned do not die with it: `tests/common/mod.rs` says so at the group-kill site — *"The deck's own lazy-spawned daemon setsid's into a separate session and escapes this group — its `DOT_AGENT_DECK_TEST_MAX_LIFETIME_SECS` cap is the net for that."* `src/daemon_attach.rs` runs `setsid(2)` in `pre_exec` so the daemon survives its parent, and a *wrapped* agent sits two `setsid` levels down, in a group the deck cannot signal at all.

So after nextest SIGKILLs a test, an orphan can keep writing under the dead test's root for up to that cap (**300s**). Reaping a dead owner the instant it dies would hand `remove_dir_all` a live process's working directory — and the moment a developer is most likely to reach for `--apply` is right after a run died, which is exactly when orphans are still alive.

The old age-only rule never had this problem: its 6h floor was 72× the orphan cap. The 10-minute floor restores that protection at 2× the cap, which is ample, and costs #461 nothing — the case it was filed for was 280 roots whose youngest was 4h09m. Note it is a timestamp on the **keep** side: that is the safe bias, and the exact inverse of the rejected PID-recycling comparison below, which biased toward deletion.

### Why there is no recycled-PID branch

Issue #461 as filed called for a fourth branch: a live PID *proven* to have started after the root already existed cannot be its creator, so the number was recycled and age should decide. That branch was built twice and removed. It is a deliberate, documented deviation from the issue — do not reinstate it.

Both attempts failed in the same direction: deleting a live run's working directory. The first read the `/proc/<pid>` directory's mtime as a start time. It is not one — Linux instantiates the per-PID procfs inode lazily, `proc_pid_make_inode()` stamps its timestamps at instantiation and `pid_getattr()` leaves them alone, so that mtime is a proc-dentry *lookup* time that moves forward again whenever the dentry is evicted and re-looked-up, which is likeliest under exactly the memory pressure this tool exists to relieve. Any live root created before its owner's first `/proc/<pid>` lookup was classified recycled and deleted under `--apply`. The second reconstructed a real start time from field 22 of `/proc/<pid>/stat` plus `/proc/stat`'s `btime`, which is the kernel's authoritative record — and still could not be trusted, for a reason no parsing fix reaches.

The comparison is unsound in principle. Ordering a process start against a directory timestamp has to bridge through the wall clock: the directory side is only ever a stored `CLOCK_REALTIME` value, while the process side has to be reconstructed as boot time plus a tick counter. Linux's `getboottime64()` contract states outright that `settimeofday` shifts the boot time behind `btime`, and inode timestamps are never retroactively adjusted to match. So any forward clock step since a root was created — admin action, a VM clock correction, a time-sync daemon, suspend/resume — moves a *live* process's reconstructed start forward while its root's timestamp stays put. A one-hour correction is enough to make a process that started one second before creating its root look like it started 59m59s after, straight past any plausible margin. The bias lands squarely on the deletion-unsafe side, and there is no further input available here that turns it back into positive proof.

Deleting the branch is still the right trade, but the cost is **not** simply "a deferral" — an earlier version of this page claimed a PID collision is always *transient*, and that is only true within one boot. Within a boot the colliding process exits and the next run classifies the root `dead-pid` and reaps it. **Across a reboot it does not hold.** A leftover root outlives the boot, low PIDs are handed to long-lived system units early in the next one, and the root is then pinned `live-pid` for the whole life of that boot — possibly re-colliding after the next reboot. That matters most on a filesystem not cleared at boot, which is where these roots are heading (`/var/tmp`, issue #322), and those roots hold real agent credentials.

`--ignore-liveness` is the answer, and its shape is deliberate. The operator supplies the one fact the program cannot measure — that the machine rebooted, so liveness here is meaningless — and the roots then fall back to the **age rule** rather than being reaped outright. That keeps the judgement out of the code, which is the whole point of deleting the inferred branch. Do not reach for a boot-time comparison to automate it: `/proc/stat`'s `btime` is subject to exactly the `settimeofday` skew described above.

For the same reason there is no report-only "possibly recycled" annotation either. It would keep the entire `/proc` parsing surface — which is where the wrong answers came from — in exchange for a hint nobody can act on differently. What the report does instead is **list** every survivor with its path, size and age rather than only counting them, so a `live-pid` root that is really a stale collision is visible and the operator can decide.

**No behavioural test can catch a reinstatement, and the suite does not claim to.** Tripping a restored comparison needs a root whose timestamp predates its owning process's start by more than the comparison's margin, and a directory's **birth time cannot be backdated** by any portable API — a test reaches mtime and nothing else, so the only gap it can manufacture is milliseconds, far inside the five-minute margin the deleted code used and inside any plausible replacement. Restore that code verbatim and every behavioural test in `clean_tmp.rs` still passes, including the one whose name used to promise otherwise. The protection is therefore a source-level guard plus code review, and nothing stronger. `source_has_no_pid_recycling_machinery` reads the module's own source through `include_str!`, blanks out comments and literals — so the explanation above stays free to discuss the branch, and so the test's own token list does not trip it — and fails if tokens such as `.created(`, `sysconf`, `_SC_CLK_TCK`, `starttime`, `process_start_time`, `boot_time` or `Recycled` reappear in *code*. `owner_of_takes_a_name_and_a_probe_and_no_timestamp` is the compile-time half: `owner_of` takes a name and a probe and no timestamp, which is the shape that makes the comparison inexpressible, so widening the signature back breaks the build. Neither replaces reading the diff.

### Output, sizes, and what the size walk guarantees

The output attributes each decision — `dead-pid`, `live-pid`, `live-aged`, `untagged` — with per-reason counts and sizes, so "nothing to reap" says which category held the survivors back instead of restating the age threshold. **Survivors are listed individually too**, not merely counted: a `live-pid` root that is really a PID collision across a reboot is indistinguishable from a running suite's root in a bare count, and it is the one class the reaper will not settle by itself, so its path, size and age have to be on screen. Both lists are capped at 20 entries (biggest first) and say how many they dropped; the summary always counts them all. Directory names are printed escaped rather than raw, because `/tmp` is mode 1777 and the suffix of a `dad-tests-<pid>-*` name is attacker-controlled text that the *default dry run* pipes to a terminal — an OSC 52 sequence in a directory name would otherwise rewrite the reader's clipboard.

Sizes are computed by a walk bounded at 50,000 entries and depth 64 per root, because the walk now runs on kept roots too rather than only on age-eligible ones. Past the budget the walk stops and the size is printed with a `≥` marker and a footnote. Sizing is presentation only and never reaches the classifier, so a truncated size cannot change a reap/keep verdict.

Every size total saturates rather than wrapping, and this is not theoretical: `meta.len()` is the *apparent* size, a sparse file costs no blocks, and three 8-exabyte sparse files planted under a `dad-tests-*` name in a world-writable `/tmp` overflow a `u64`. That aborted the plain debug `cargo xtask clean-e2e-tmp` with `attempt to add with overflow` before it could clean anything, and a release build wrapped silently and printed a false size. `--older-than` saturates its hours-to-seconds multiplication for the same reason.

The walk stats every entry with `symlink_metadata`, so **a symlink is never descended as observed**: it contributes its own length and nothing more, and a symlink loop cannot spin the walk. That is a statement about what the walk sees, not a race-free guarantee — the older flat claim that it "never follows symlinks" was too absolute. `read_dir` resolves by path, so a local user with write access inside the tree can replace an already-observed directory with a symlink before it is opened, and the open follows the replacement; the walk re-stats each directory after opening it, which rejects a swap still in place but not one undone again inside the window. The residual consequence is bounded and presentation-only: at worst some other tree's entries are counted into a size, capped by the entry and depth budgets. Nothing from outside the candidate is ever printed, and no reap/keep verdict can move.

It is not a deletion escape either — but **not** because the collector `lstat`ed the path first. That check and the removal are separated by the whole classify-and-size pass, and in a mode-1777 `/tmp` a user can `rename(2)` their own entry into a symlink inside that window (the sticky bit does not help, since they are modifying an entry they own). The removal is safe because `std::fs::remove_dir_all` does not follow a symlink at the path it is given, and has been `openat`-based since the CVE-2022-21658 fix. Crediting the earlier `lstat` instead would license replacing it with a hand-rolled recursive delete, which is exactly where this bug class gets introduced.

`--older-than` applies to the age-decided cases only: no-usable-PID roots, and live-PID roots under `--ignore-liveness`. A dead-owner root is governed by its own 10-minute floor instead, and a live-PID root by nothing at all.

## Pre-flight space check

Under `--features e2e`, the harness checks free space on the temp filesystem before allocating its root and fails with an explicit message naming both the shortfall and the remedy. This exists because tmpfs exhaustion does *not* look like an out-of-space error — it surfaces as agents never becoming input-ready, `git init` failing, and daemons never booting, which reads like a product regression. One diagnosis of this cost a full round trip.

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `TMPDIR` | system temp | Where the harness root is allocated. Honoured via `std::env::temp_dir()`; the harness creates the directory if it does not exist, so pointing it under `target/` survives `cargo clean`. |
| `DAD_E2E_MIN_FREE_MB` | `2048` | Free space the e2e tier requires. `0` disables the check. |
| `DAD_E2E_IMPORT_CLAUDE_PLUGINS` | unset (off) | Set to `1` to copy the host's `~/.claude/plugins` into every seeded HOME. Off by default: it is ~11 MB per HOME, nothing in the suite depends on it, and with dozens of tests running concurrently it is a real share of peak temp demand. |

## A note on tmpfs

If `/tmp` on your machine is a tmpfs, every leftover is resident memory rather than disk, and the failure mode is self-amplifying — a run that dies mid-test leaves more behind, so the next run has less headroom. Pointing `TMPDIR` at a directory under `target/` avoids both the RAM cost and the size ceiling; measured on an NVMe machine, doing so cost no measurable wall-clock time (fast tier 24.5s either way). It is not the default because it trades a loud failure for a silent one: leaks stop causing red runs and instead just accumulate.
