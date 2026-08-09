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

Only pass `--include-untagged` when no other Rust build or tool is running; it can otherwise delete a live temp dir belonging to something else. Symlinks are never followed.

### Ownership decides, age is only the fallback

The `<pid>` in `dad-tests-<pid>-<random>` is the process that created the root, so it answers "is anyone still using this?" directly, where age is only a proxy. Since [issue #461](https://github.com/vfarcic/dot-agent-deck/issues/461) the reaper reads it back:

| The root's PID | Decision |
|---|---|
| No live process holds it | **Reap, at any age.** `--older-than` does not hold it back. |
| Held by a live process | **Keep, at any age.** |
| Held by a live process that started *after* the root's mtime | The number was recycled and proves nothing — decided by age. |
| Not readable (untagged `.tmp*`, a lock dir, a malformed name, or a non-Unix host) | Decided by age. |

Liveness is `kill(pid, 0)`, in which `EPERM` counts as **alive** — the process exists, it merely belongs to another user — and only `ESRCH` means dead. Start time comes from the `/proc/<pid>` directory's mtime; where it cannot be read the live process is assumed to be the owner and the root is kept.

Both directions of that change matter. Age alone once refused 280 roots totalling **6.2 GB**, every one with a dead owner, because the oldest was 4h09m and the default threshold is 6h — on a 14 GB tmpfs with 5 MiB of swap left and an e2e compile about to start. It also made the opposite mistake: a suite still running past the threshold was eligible to have its own live root deleted out from under it.

The output attributes each decision — `dead-pid`, `live-pid`, `recycled`, `untagged` — with per-reason counts and sizes, so "nothing to reap" now says which category held the survivors back instead of restating the age threshold. The per-directory list is capped at 20 entries (biggest first) and says how many it dropped; the summary always counts them all.

`--older-than` still applies to the fallback cases only.

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
