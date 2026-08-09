# E2E temp directories

How the test harness allocates scratch space, why it used to leak, and what to run when a machine has accumulated leftovers. Background: [issue #322](https://github.com/vfarcic/dot-agent-deck/issues/322).

## The one-root rule

Every temp directory allocated **inside a test process** nests under a single per-process root named `dad-tests-<pid>-<random>`, created on first use in the [temp base](#where-the-root-lives) — by default `/var/tmp/dad-e2e-<uid>`. `race_safe_tempdir()` and the daemon lock dir go through it explicitly, and a bare `tempfile::tempdir()` lands there too because `harness_temp_root()` points the `tempfile` crate's *default* directory at the root once it exists (`tempfile::env::override_temp_dir`). So there is exactly one place to look and exactly one thing to clean up.

Two things are outside that rule and are not claimed to be inside it. **Processes the harness spawns** — agents, daemons, `git`, `gh` — resolve temp the way the OS tells them to, which is `TMPDIR`, unchanged; the redirect above is `tempfile`'s own process-global default, not an environment variable, precisely so it does not silently reach into every child. And a `tempfile::tempdir()` in a test crate that never touches the harness at all (`tests/rehydration.rs`, `tests/pane_close.rs`, `tests/daemon_protocol.rs`) still uses the system temp dir; those are small L1 allocations with a `TempDir` guard, not the repo clones that motivated this.

The root is removed by an `atexit(3)` hook when the test process exits normally. The hook retries a few times before giving up, because a daemon or agent the test spawned can outlive the test body for a moment and keep writing into the tree, making the first sweep lose a race. If it still cannot remove the root it says so on stderr rather than failing silently.

A process that is **SIGKILLed** never reaches the hook at all and leaves its root behind. That is the one remaining leak path and it is what the reaper below exists for. Measured on a full `cargo test-e2e` run of 3,347 tests: **16 roots totalling ~360 MB**, down from 46 before the retry was added. Running the same tests in isolation leaks nothing, so this is a symptom of parallel load, related to the contention described in [#351](https://github.com/vfarcic/dot-agent-deck/issues/351).

Two properties matter and are easy to break if you touch this code:

- **The root must be created through the single choke point.** `harness_temp_root()` is where the pre-flight space check runs, where the root is created 0o700, and where the `tempfile` redirect is installed.
- **The name must stay distinctive.** The `tempfile` crate's *default* prefix is `.tmp`, so `/tmp/.tmp*` belongs to every Rust program on the machine. The reaper can only safely delete names this repo owns, which is why the root is not simply another `.tmp*` dir.

## Where the root lives

The temp base defaults to `/var/tmp/dad-e2e-<uid>` — **not** `/tmp`, and deliberately **not** anywhere under the checkout.

The reason it is not `/tmp` is that `/tmp` is a tmpfs on this project's dev box, so every leftover root is resident RAM rather than disk. Measured on `main` at `d3ea031`: 280 leaked roots totalling 6.2 GB accumulated in four hours, with swap down to **5 MiB free**; reaping them returned 3.8 GiB of swap, which is headroom `rustc` needs for the e2e compile. The failure that follows does not look like an out-of-space error — `dispatch_013` went **122s FAIL → PASS in 8.9s** with nothing changed but the temp location.

`/var/tmp` is the replacement because it is short, and because the FHS requires it to survive reboots, which in practice means distributions do not back it with a tmpfs. That is a **convention, not a runtime guarantee**: nothing here calls `statfs` to check the filesystem type, so a machine that has deliberately mounted `/var/tmp` on tmpfs gets the old behaviour back with no warning. The same caveat applies to any `DAD_E2E_TMPDIR` you set. `df -h /var/tmp` and `findmnt -T /var/tmp` are the two commands that actually answer the question.

### Why not `target/`

Putting the base under the repo's own `target/` was the first attempt at this fix, and it is worse than it looks. Every seeded fixture would then be a **descendant of the real checkout**, which carries `CLAUDE.md`, `AGENTS.md`, `.claude/` and `.agents/`. Real agents walk ancestors and would pick up genuine project instructions and skills, and the Codex worker runs `workspace-write` from that directory — so a test's effective writable workspace could be the live repository. A nested `git init` does not close it: a git root is not a filesystem boundary, and several real-agent tests (`e2e_delegate_work_done_chain`, `e2e_pi_worker`, `e2e_codex_worker`, `e2e_pi_orchestrator`) call `race_safe_tempdir()` with no `git init` anywhere near them. If you specifically want a target-local base, set `DAD_E2E_TMPDIR` to one — that is an explicit choice, and it is not made for you.

### Why a private parent rather than `/var/tmp` itself

`/var/tmp` is mode **1777** — world-writable, sticky, shared by every user on the machine. Roots placed directly in it are indistinguishable by name from a `dad-tests-*` directory belonging to somebody else, which makes both halves of the problem unpleasant: a reaper that trusts the name can erase another user's credential-bearing sandbox, and one that does not usually just fails. Nesting everything inside a parent that is verified to be **0700 and owned by the effective UID** removes the question by construction — nothing under it can belong to another user.

The parent is created with the mode applied by `mkdir(2)` itself, never chmod'ed afterwards. If it already exists it is **verified, not repaired**: not a symlink, owned by us, no group or other bits. A parent that fails verification is left exactly as it is and the harness **stops** — it does not fall through to the next candidate, because that candidate is usually the RAM-backed system temp dir and quietly landing there turns a security refusal back into the capacity problem this whole page is about. The failure names the path, what was observed (owner and mode) against what is required, and the remedy (`ls -ld` then `rm -rf`, or point `DAD_E2E_TMPDIR` somewhere else). A parent that is merely **absent** — no `/var/tmp` at all, a non-Unix platform, a read-only filesystem — is an ordinary environment difference and still falls through with a warning; only a directory that is *present and untrustworthy* is fatal.

### The ladder

The one thing that can veto a candidate is **path length**. These directories hold Unix domain sockets, and `sockaddr_un::sun_path` caps at 108 bytes on Linux and 104 on macOS/BSD. `/tmp` costs 4 characters where a `<worktree>/target/tmp` costs 60+, and this repo's worktree scheme (`../<repo>-<suffix>`, used by `/worktree-prd` and `/verify-pr`) reaches that routinely — in `dot-agent-deck-dispatch-tmpfs-322` an `attach.sock` at the harness's usual depth is 115 bytes and `bind(2)` fails with `AF_UNIX path too long`. `/var/tmp/dad-e2e-1000` is 21 bytes against a 55-byte allowance, so it has 34 to spare.

| # | Candidate | Notes |
|---|---|---|
| 1 | `$DAD_E2E_TMPDIR` | Explicit. Validated (see below), then honoured — including when it is too deep for a socket, which warns rather than silently relocating. A value that fails validation is fatal, never demoted to candidate 2. |
| 2 | `/var/tmp/dad-e2e-<uid>` | The default. Unix only; created 0700 or verified as ours. Absent means fall through; present but untrustworthy is fatal. |
| 3 | `std::env::temp_dir()` (`TMPDIR`, else `/tmp`) | Last resort, and the only rung on Windows. This is the one outcome that can put the suite back on a RAM-backed filesystem, so it always prints a warning. |

`TMPDIR` on its own no longer relocates the harness root — it only reaches candidate 3. Use `DAD_E2E_TMPDIR` to move the harness deliberately. `CARGO_TARGET_DIR` has no effect on the temp base at all any more.

### What `DAD_E2E_TMPDIR` is checked for

It is not taken verbatim. It must be **absolute** and free of `..` — a relative value would resolve against whatever working directory a test binary happens to have, and `..` silently widens the scope of everything downstream. Every component that already exists is stat'ed once and refused if it is a **symlink** (pathname resolution follows links in ancestors on *every* use, so one in a writable location can be re-pointed between two uses of the same path), if it is owned by **another unprivileged user**, or if it is **group/world-writable without the sticky bit**. Sticky 1777 directories such as `/tmp` and `/var/tmp` are accepted as ancestors: the sticky bit is exactly the guarantee that only an entry's owner may rename or remove it. Anything missing is then created 0700, one component at a time.

A rejected value **is** fatal, and more plainly so than a rejected default: setting the variable states where the temp dirs must go, so a value that cannot be honoured — for any reason, including "could not be created" — stops the harness rather than being ignored. There is no reading of an explicit instruction under which silently doing something else is the helpful answer.

## Why an `atexit` hook rather than `Drop`

The lock dir this replaced was held in a `static OnceLock<TempDir>`. **Rust does not run destructors for statics at process exit**, so its `TempDir::drop` never fired. Because nextest runs one process per *test*, that leaked one directory per test — on a fully green run. Measured before the fix: 13 directories from 56 passing tests, and 6,667 accumulated on one dev machine over eight days.

If you ever need process-lifetime scratch state, do not reach for a `static TempDir`. Put it under the harness root and let the exit hook take it.

## Reading a leftover: 0775 means the sweep *worked*

A leftover `dad-tests-*` directory at mode **0775 is not a harness root**. The harness creates its root with `mkdir(2)` at 0700 and panics if the mode it reads back is anything looser, so a root it created can never be observed at 0775. What you are looking at is a *re-creation*: the exit hook removed the real root, and an agent process the test spawned outlived it and wrote into the `$HOME` it still had — `mkdir -p` walked the deleted chain back into existence at the umask default (0775 under the common `umask 002`).

The forensic signature, all four of which held for every one of the 32 leftovers observed after one full e2e run:

- **mode 0775** on the root *and* on the `.tmpXXXXXX` per-test dir inside it — both are created 0700 and both are asserted, so neither can be a survivor;
- **no fixture content** — the fixture copy is the first thing that happens after the per-test dir is created, so a genuine root always has it; a skeleton has only the subtree the orphan re-made (typically just `home/`);
- the root's **birth time equals its mtime**, meaning nothing was ever created in it after the single child that re-made it;
- the root's **birth time equals the inner dir's birth time**, because one `mkdir -p` made both.

A root left by an abnormal termination looks like the opposite of all four: `SIGKILL` skips the exit hook, so the whole tree survives at 0700 with the fixture, `.git` and the seeded `HOME` intact.

The distinction matters when you are judging whether cleanup regressed. Skeleton residue is evidence the sweep **ran** — the fixture is gone precisely because it succeeded — and it cannot be fixed by making the sweep more reliable, only by reaping the spawned processes before the sweep or re-sweeping after them. That is the leak-*rate* question tracked separately in #461. Either way the residue stays reclaimable: the reaper keys on the directory *name*, never on its mode.

There is no live exposure while this sits on disk. The private parent is 0700, so no other user can traverse into it whatever the modes inside say.

## Reaping leftovers

```bash
cargo xtask clean-e2e-tmp                      # dry run — always start here
cargo xtask clean-e2e-tmp --apply              # actually delete
cargo xtask clean-e2e-tmp --older-than 1 --apply
cargo xtask clean-e2e-tmp --root /my/base --apply   # a base you moved yourself
```

Dry-run is the default and `--apply` is required to delete anything. By default it only considers directories this repo owns:

| Prefix | Reaped by default | Why |
|---|---|---|
| `dad-tests-*` | yes | The current harness root. |
| `dot-agent-deck-test-lock-*` | yes | Pre-fix lock dirs, still present in bulk on older machines. |
| `.tmp*` | **no** — needs `--include-untagged` | The `tempfile` crate's default prefix, shared with every Rust program on the machine. |

Only pass `--include-untagged` when no other Rust build or tool is running; it can otherwise delete a live temp dir belonging to something else. Because of that it is **restricted to the system temp dir** (the historical location the advice was written for) and to any directory you name with `--root`. It never applies to the private `/var/tmp` parent. Directories younger than the age threshold (default 6h) are always left alone so a reap cannot race a running suite, and symlinks are never followed.

### Which directories it looks in

The **standard** roots: the private `/var/tmp/dad-e2e-<uid>` parent and the system temp dir. Roots that are absent are skipped silently; two spellings of one directory (a symlink, a `TMPDIR` with a trailing `/.`) are de-duplicated by the directory they resolve to, not by how they are written.

That is *this* machine's, *this* checkout's picture. It cannot infer another worktree's leftovers, or a `DAD_E2E_TMPDIR` that is no longer exported — **run it in the worktree the leaking run ran in**, or name the directory with `--root`.

`--root` is also how you reap a base you moved with `DAD_E2E_TMPDIR`, which is deliberately *not* scanned automatically: where the harness may write and what a delete command may remove are different trust decisions, and one should not silently grant the other. When the variable is set but not passed, the reaper prints a note naming it and showing the `--root` invocation. Passing `--root` **replaces** the standard set rather than adding to it, so a deliberate scan of one directory cannot quietly also delete from `/var/tmp` or `/tmp`.

## Pre-flight space check

Under `--features e2e`, the harness checks free space on the temp base it actually chose — not a hardcoded `/tmp` — before allocating its root, and fails with a message that leads with `HARNESS PRE-FLIGHT FAILURE … NOT a product regression` and names the path, the requirement, the shortfall and the remedy (including the `--root` form, since a base you moved yourself is not one the reaper scans by default). This exists because an exhausted temp filesystem does *not* look like an out-of-space error — it surfaces as agents never becoming input-ready, `git init` failing, and daemons never booting, which reads like a product regression. One diagnosis of this cost a full round trip.

It is one `statvfs` per test process, at the single choke point every harness temp dir passes through. It is also deliberately incapable of becoming a new flake: a filesystem whose free space cannot be queried produces no verdict at all, and `DAD_E2E_MIN_FREE_MB=0` switches it off entirely.

The 2 GB default is a "this run is doomed" floor rather than a capacity guarantee. Peak demand is what matters — one seeded HOME measures 263–284 MB and nextest runs one process per core, so eight concurrent tests already want ~2.2 GB — but the threshold is set below true peak so it catches the exhausted-tmpfs case without tripping on a modest CI runner.

## Environment variables

| Variable | Default | Effect |
|---|---|---|
| `DAD_E2E_TMPDIR` | unset | Temp base for the harness root. Validated (absolute, no `..`, no symlinked or foreign-owned component), then outranks every other candidate — including the socket-length veto, which only warns. A value that fails validation stops the harness rather than being ignored. |
| `TMPDIR` | system temp | Only reaches the last-resort candidate (3). It no longer relocates the harness root on its own — use `DAD_E2E_TMPDIR`. |
| `DAD_E2E_MIN_FREE_MB` | `2048` | Free space the e2e tier requires on the chosen base. `0` disables the check. |
| `DAD_E2E_IMPORT_CLAUDE_PLUGINS` | unset (off) | Set to `1` to copy the host's `~/.claude/plugins` into every seeded HOME. Off by default: it is ~11 MB per HOME, nothing in the suite depends on it, and with dozens of tests running concurrently it is a real share of peak temp demand. |

`CARGO_TARGET_DIR` no longer affects any of this.

## Leftovers hold real credentials

The harness copies the host's agent auth state into every seeded HOME so real-agent tests can run (issue #358 tracks narrowing that). Cross-user read access is blocked — leaves are written 0600, the root is 0700, and the `/var/tmp` parent is 0700 — but the *lifetime* changed with the move: `/tmp` is usually cleared at boot, `/var/tmp` is required not to be. A SIGKILLed run therefore leaves real Claude/OpenCode/Codex auth state on durable storage until something removes it.

The expectation is that leftovers do not outlive a working day: run `cargo xtask clean-e2e-tmp --apply` at the end of a session where the suite was interrupted, and treat anything older than the 6h default threshold as something to reap rather than something to leave. It is a retention expectation, not an enforced one — nothing expires those directories on its own.

## A note on tmpfs

If `/tmp` on your machine is a tmpfs, every leftover is resident memory rather than disk, and the failure mode is self-amplifying — a run that dies mid-test leaves more behind, so the next run has less headroom. That is why the default base moved off it. Measured on an NVMe machine, moving the suite off tmpfs cost no measurable wall-clock time (fast tier 24.5s either way).

The tradeoff this default accepts is that leaks stop causing red runs and instead accumulate quietly. That is what the reaper and the pre-flight check above are for — run `cargo xtask clean-e2e-tmp` occasionally rather than waiting for a suite to go red.
