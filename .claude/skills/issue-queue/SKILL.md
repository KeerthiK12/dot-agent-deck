---
name: issue-queue
description: Build the queue of open issues that are actually available to work — excluding PRDs, anything already in flight, and duplicates — then assign them and dispatch one isolated agent per issue. Asks how many to take, verifies each candidate against origin/main rather than the local checkout, and composes a self-contained task carrying this repo's gates. Use when asked to find issues to work on, pick something off the backlog, or work through several issues in parallel. It does no implementing itself — for one named issue, just work it directly.
user-invocable: true
---

# Dispatch the issue queue

Sibling of `pr-review-queue`, for issues rather than PRs. Same discipline: select, ask, assign, dispatch, report. The work happens inside dispatched units, never here.

## When to use this

The backlog is large and the question is *what is genuinely available to work on right now, and can several be worked in parallel*. This skill answers that and starts the work.

Not this skill:

- **One issue, named, that you intend to fix now** → just fix it. Dispatching a single unit adds a worktree between you and the change.
- **A PRD** → `/prd-start` or `/prd-full`. PRDs are excluded from this queue by construction (see step 2).
- **PRs rather than issues** → `/pr-review-queue`.

## What this skill does NOT do

It **never implements a fix, and never diagnoses beyond what selection requires**. Reading an issue body to judge scope is in bounds; reading source to design the fix is not. If you are editing files under `src/`, you have left this skill.

## Step 0 — Fetch, and never trust the working tree

**Run `git fetch origin` before verifying anything, and verify against `origin/main`, not the checkout.**

```bash
git fetch origin --quiet
git rev-list --left-right --count HEAD...origin/main   # "0  12" means 12 behind
```

This is not hygiene, it is correctness. Measured on 2026-08-11: a local `main` sitting **12 commits behind** `origin/main` made `grep -rn shell_foreground_busy_snapshot src/` return nothing for code that had merged the previous day, which came within one step of a report that two valid issues referenced code that did not exist. A stale checkout does not fail loudly — it silently reports every recently-added symbol as absent, so *every* "still unfixed" conclusion inverts.

So verify claims with `git grep` against the remote ref:

```bash
git grep -n "fn shell_foreground_busy_snapshot" origin/main -- src/agent_pty.rs
```

Do **not** `git pull` to fix this. The runner may have local work, and this skill has no business moving their branch.

## Step 1 — Resolve identity at runtime

```bash
ME=$(gh api user --jq .login)
read -r OWNER REPO < <(gh repo view --json owner,name --jq '"\(.owner.login) \(.name)"')
```

Never hardcode a login. This repo has two maintainers ([@vfarcic](https://github.com/vfarcic) and [@prageethw](https://github.com/prageethw)) and a hardcoded one silently hands the other person somebody else's queue.

## Step 2 — Select candidates

**The rule: open issues that are unassigned or assigned to the runner, excluding PRDs.**

```bash
gh issue list --state open --limit 300 --json number,title,labels,assignees,createdAt \
  | jq -r --arg me "$ME" '.[]
    | select((.labels|map(.name)|index("PRD"))==null)
    | select((.assignees|length)==0 or ([.assignees[].login]|index($me)))
    | "\(.number)\t[\([.labels[].name]|join(","))]\t\(.title)"'
```

Two notes on the filters:

- **`--limit` is a bound you must act on, not a disclaimer.** `gh issue list` silently truncates. If the count comes back equal to the limit, raise it and re-run; a truncated queue looks like a complete one.
- **PRD exclusion is by label, not by title.** Some PRD issues have titles that start with "PRD:" and some do not (#381 does not); the `PRD` label is the reliable signal.
- **Assignment on this repo is currently sparse** — on 2026-08-11, 0 of 110 open non-PRD issues had any assignee, so the assignee filter admitted everything. Do not conclude from that that the filter is useless; it is what keeps two maintainers from colliding once assignment is in use, which is the point of step 4.

## Step 3 — Eliminate what is already in flight

**This step is why the skill exists.** Skipping it wasted an agent on 2026-08-11: #490 was dispatched into a bug already being fixed on `agent/dispatch-fix-skip-detection`, producing PR #496 as a straight duplicate of PR #495. Both declare the same closing refs.

Three independent checks, because no one of them is sufficient.

**3a. PRs that declare a closing reference.** Catches the majority:

```bash
gh api graphql -f query='
query($owner:String!, $repo:String!) {
  repository(owner:$owner, name:$repo) {
    pullRequests(states:OPEN, first:50) {
      nodes {
        number title headRefName
        closingIssuesReferences(first:10) { nodes { number } }
      }
    }
  }
}' -F owner="$OWNER" -F repo="$REPO" \
  --jq '.data.repository.pullRequests.nodes[]
        | select(.closingIssuesReferences.nodes|length>0)
        | "PR #\(.number) [\(.headRefName)] closes: \([.closingIssuesReferences.nodes[].number]|join(", "))"'
```

**3b. PRs that fix an issue without declaring it.** `closingIssuesReferences` only sees explicit `Fixes #N` / `Closes #N` keywords, so a PR that solves an issue while describing it in prose is **invisible to 3a**. Measured: PR #466 ("make a failed delegate loud") implements exactly the fix proposed in #309 and #330 and appears in no closing-refs output, because it names neither. So also scan open PR titles and bodies for the candidate's subject matter before dispatching at it:

```bash
gh pr list --state open --json number,title,body,headRefName \
  | jq -r '.[] | "#\(.number) [\(.headRefName)] \(.title)"'
```

Read the bodies of any whose title is in the same area as a candidate. There is no mechanical substitute for this; the cost of skipping it is a duplicate PR.

**3c. Dispatch branches and worktrees, including ones with no PR yet.** An agent that has started but not yet pushed is invisible to both queries above:

```bash
git branch -a --list '*dispatch*' | sed 's/^..//' | sort -u
ls -d ../*-dispatch-* 2>/dev/null
```

A branch whose worktree is gone is *finished or abandoned* work, not in-flight — but its **name is still taken** (see step 6).

## Step 4 — Detect duplicate issues, and pair coupled ones

**Duplicates.** This backlog carries duplicate pairs filed from separate verification sessions — #470/#489 (same `--workspace` test-gate gap) and #452/#490 (same anchored-grep bug) were both live on 2026-08-11. Cluster candidates by subject before presenting, and when dispatching one, put "close #N as a duplicate" **in the task text** so the unit's PR closes both. Two agents on one bug is the failure this prevents.

**Coupling.** Issues that touch the same function must be dispatched as **one unit**, not two. Two agents editing `handle_work_done` in separate worktrees produce a guaranteed conflict and two half-fixes. Known couplings at time of writing: #448+#433 (both `handle_work_done`), #493+#429 (both `shell_foreground_busy_snapshot`). Check for this by grepping the issue bodies for the same file and symbol names.

Prefer picks whose file sets are **disjoint from the other units in the same batch**. When two candidates are equally good, the tiebreak is which one shares fewer files with what is already dispatched.

## Step 5 — Show the queue, then ask how many

Print the candidates with **number, labels, title, a one-line scope read, and any duplicate or coupling note**. Show what was excluded and why — in-flight exclusions especially, since that is where the runner is most likely to know something the queries cannot see.

Then **ask how many to dispatch, recommending 2–3.** Do not assume "all", and do not offer "all" as the recommendation. Each unit runs the full gate chain including `cargo test-e2e`, the most expensive gate in the repo (CLAUDE.md rule 5), and CLAUDE.md rule 14 records how concurrent multi-GB `target/` trees surface as a misleading `linking with 'cc' failed` or a `SIGKILL` on `rustc`. An agent hitting either will blame its issue rather than the batch size.

Ask **which** issues too, unless the runner already named them. Relative value is theirs to judge; a security issue and a 2 Hz polling inefficiency are not interchangeable just because both are small.

## Step 6 — Assign before dispatching

**Assign the runner to every issue being dispatched, unless it already has an assignee.** This is a hard step, not a courtesy — it is what stops the other maintainer from starting the same work, and the whole point of dispatching is that nobody is watching the issue while the unit runs.

```bash
gh issue edit <n> --add-assignee "$ME"
```

Never reassign an issue that already has someone on it: that is a collision to report to the runner, not to resolve. If a candidate is assigned to the *other* maintainer, it should not have reached this step — step 2's filter excludes it.

Verify the write landed before dispatching; `gh issue edit` can succeed against a read-only token in ways that do not surface here.

Then check the dispatch name is free. A name is single-use, and **removing a worktree keeps its branch**, so `agent/dispatch-<name>` surviving from finished work refuses a re-dispatch. Pick an unused name or delete the stale branch.

## Step 7 — Dispatch, one unit per issue

Ask `--single` vs `--orchestration` per the dispatch contract, then:

```bash
dot-agent-deck dispatch <name> --single --task "<self-contained text>"
```

**The task text must be self-contained with respect to the conversation, not the repo.** The unit gets a copy of this repo, so reference paths, skills and issue numbers rather than pasting contents. Each task should carry:

- **The issue number and `gh issue view <n>`** for the full analysis — do not restate what the issue already argues.
- **The goal in one or two sentences**, and the expected end state.
- **Any duplicate to close** and any coupled issue included in the unit.
- **The non-obvious constraint**, where the issue records one. These are the most load-bearing sentences in the task, because they are what an agent reading only the code would get wrong — e.g. #429's "a timed-out sample must yield `None`, never `Some(false)`", or #448's "`DelegationRetirement::Nothing` is not a reliable proxy for never-delegated".
- **The gates, from CLAUDE.md**: `cargo fmt --check` and `cargo clippy --workspace --all-targets --features e2e -- -D warnings` before every commit, `cargo test-fast` per task, `cargo test-e2e` before the PR.
- **A changelog fragment** via the `dot-ai-changelog-fragment` skill.
- **CLAUDE.md rule 12** where the change touches the daemon, protocol, orchestration or hooks: the unit must answer the `PROTOCOL_VERSION`-vs-`.breaking.md` question explicitly rather than silently.
- **Rule 4** where the change is user-visible: which test tier it needs.
- **A stop instruction**: open the PR, request review from the other maintainer, and **stop**. Per CLAUDE.md rule 8 nobody merges their own unapproved PR, and for the admin that would succeed silently rather than fail.

Re-check the issue's state **immediately before each dispatch**, not once for the batch. Issues move: on 2026-08-11 three PRs appeared for this queue's own issues within minutes of dispatch.

## Step 8 — Report where the work went

Give the runner, per unit: issue number, worktree path as `dispatch` reported it, and branch. Then state plainly:

- **Nothing reports back to this pane.** `dispatch` is fire-and-forget with no return edge. Point at the worktree paths and the units' own tabs; never say results will arrive here.
- **Anything you excluded, and why** — especially in-flight collisions and duplicates.
- **Anything you could not verify**, including a stale local checkout you chose not to move.
