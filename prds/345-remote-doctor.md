# PRD #345: `remote doctor` — diagnose remote connectivity and ssh forwards

**Status**: Not Started
**Priority**: Medium
**Created**: 2026-08-02
**GitHub Issue**: [#345](https://github.com/vfarcic/dot-agent-deck/issues/345)
**Related**: [#97](https://github.com/vfarcic/dot-agent-deck/issues/97) (the reverse-tunnel recipe this exists to make debuggable), [#344](https://github.com/vfarcic/dot-agent-deck/issues/344) (`ForwardFailed` classification — the error-path half of the same problem), `src/connect.rs` (`probe_remote_version`, `probe_remote_protocol`, `map_probe_ssh_error`, `RemoteConnectError`), `src/remote.rs` (`SystemSshExecutor::build_command`, `RemoteEntry`, `SshTarget`), [`docs/remote-recipes.md`](../docs/remote-recipes.md#reaching-networks-only-your-laptop-can-see)

## Problem Statement

The recipe added for #97 tells users to configure reverse tunnels in `~/.ssh/config` so a remote can reach networks only the laptop can see. It works — but every way it can fail is currently opaque, and several were discovered only by building the setup and testing it:

1. **`AllowTcpForwarding no` on the remote and a port collision are indistinguishable.** Both produce exactly `Error: remote port forwarding failed for listen port N` on the client. Alpine's `openssh` package ships `AllowTcpForwarding no`, as do most hardening baselines, so this is a live first-run failure and not a corner case.
2. **A failed forward can be silent.** Without `ExitOnForwardFailure yes`, ssh brings the session up anyway and the forward simply does not exist. Agents then fail on network access with errors that point at git, not at the tunnel.
3. **`DynamicForward` looks right and does nothing useful.** It puts the SOCKS listener on the laptop rather than the remote. This exact mistake appeared in the original #97 proposal *and* in the maintainer's reply agreeing with it, which is strong evidence users will make it too. Nothing in the current failure output would tell them.
4. **The ssh config applies to the deck's own probes.** A forward listener that has not been reaped yet makes the version probe fail, which the deck reports as an unreachable host and then burns its reconnect budget against (see #344).

The deck already owns the registry and the ssh transport. What it has never offered is an answer to "is this remote actually set up the way I think it is?" — so today the only diagnostic path is reading `man ssh_config` and guessing.

## Key insight: `ssh -G` makes this cheap and non-invasive

`ssh -G <destination>` prints ssh's *resolved* configuration for that destination, including forwards, in a stable machine-readable form. Verified directly:

```
exitonforwardfailure yes
dynamicforward 9099
remoteforward 1080 [socks]:0
```

Note that ssh labels reverse-dynamic (SOCKS) forwards unambiguously as `[socks]:0`, so the correct and incorrect configurations are trivially distinguishable without parsing anything ourselves.

This matters because it sidesteps the objection that killed the `ssh_args` proposal in #97: the deck does **not** parse `~/.ssh/config`, does **not** store forwarding configuration, and does **not** fork ssh's option grammar. ssh remains the single source of truth; the deck only reads what ssh already decided. Diagnosing the user's infrastructure is a different proposition from owning it.

## Solution

A read-only `dot-agent-deck remote doctor <name>` that runs a fixed list of checks and reports each as PASS / WARN / FAIL with a specific fix. No new state, no new configuration surface, no mutation of anything — it probes and reports.

Checks, roughly in dependency order:

| Check | Source | Catches |
|---|---|---|
| Host reachable, auth works | existing probe path | the ordinary broken-ssh case |
| Binary present, protocol compatible | `probe_remote_version` / `probe_remote_protocol` | drift the deck already classifies |
| Resolved forwards inventory | `ssh -G` | shows the user what ssh actually resolved, which may not be what they wrote |
| `dynamicforward` present | `ssh -G` | the wrong-direction mistake (#3 above) |
| `exitonforwardfailure` unset | `ssh -G` | silent forward failures (#2 above) |
| Remote `AllowTcpForwarding` | `sshd -T` on the remote | the indistinguishable-error case (#1 above) |
| Remote `ClientAliveInterval` | `sshd -T` on the remote | stale listeners orphaned by laptop sleep (#4 above) |
| Forward actually bound | probe the remote's loopback | a forward that failed despite looking configured |
| `forwardagent yes` | `ssh -G` | security advisory — every agent on the remote can use the laptop's ssh-agent |

The last one is an advisory, not a failure: it is a legitimate choice, just one the docs recommend against.

### Relationship to #344

#344 fixes the *error path* — classifying a forward failure as `ForwardFailed` instead of `HostUnreachable` when a connect attempt fails. This PRD fixes the *inspection path* — letting a user ask the question before or after a failure and get a specific answer. They are complementary and independent: #344 is ~15 LOC in an existing enum and should land first, since it is a wrong message today regardless of whether this command is ever built.

Deliberately out of scope for #344 and in scope here: distinguishing *which* cause produced a forward failure. That requires inspecting the remote's sshd config, which belongs in a diagnostic, not in an error constructor.

## Decisions

- **No experimental flag.** Decided during PRD creation, per CLAUDE.md rule 9. A diagnostic's entire value is being reachable when someone is already stuck, and a user debugging a broken tunnel will not have `experimental = true` set — gating it would hide the command from exactly the population it exists for. It is also read-only, so the usual reason to gate (unfinished behavior touching real state) does not apply. Output format can still be iterated after ship.
- **No cross-version contract impact.** This touches neither the daemon, the TUI↔daemon protocol, orchestration, nor hooks — it is a CLI command that shells out over ssh. No `PROTOCOL_VERSION` bump and no `.breaking.md` fragment (CLAUDE.md rule 12). Ships as a patch-level feature.
- **`ssh -G` rather than reading `~/.ssh/config`.** See "Key insight" above.

## Milestones

- [ ] **M1 — Command skeleton with the checks the deck already knows how to do.** `remote doctor <name>` exists, resolves the registry entry, and reports reachability / binary / protocol using the existing probe functions. Useful on its own for the ordinary "why won't this connect" case.
- [ ] **M2 — Forward inventory from `ssh -G`.** Parses the resolved forwarding directives and reports what ssh actually resolved for that destination, including the `[socks]:0` reverse-dynamic form.
- [ ] **M3 — Remote-side sshd checks.** Reads `AllowTcpForwarding` and `ClientAliveInterval` from `sshd -T` on the remote and reports the two causes that are indistinguishable from the client today.
- [ ] **M4 — Live forward liveness and advisories.** Probes whether a configured forward is actually bound on the remote; emits the `DynamicForward` wrong-direction and `ForwardAgent` advisories.
- [ ] **M5 — Tests.** Fast-tier coverage for `ssh -G` output parsing and check classification against captured fixtures. An e2e test can reuse the container harness written during #97 validation (sshd in a container, a laptop-loopback-only service) — that harness already reproduces every failure mode above deterministically.
- [ ] **M6 — Documentation.** A troubleshooting entry wired into the #97 recipe's "Limits worth knowing" section, so the caveats and the command that checks them are cross-referenced.

## Success Criteria

- A user who has followed the #97 recipe and whose tunnel does not work can run one command and be told which of the known causes applies, with the fix.
- Each of the four failure modes in the Problem Statement is distinguishable in the output — in particular `AllowTcpForwarding no` versus a port collision, which no amount of client-side error text can currently separate.
- The command never mutates ssh config, sshd config, the registry, or the remote.

## Risks

- **Remote-side checks need `sshd -T`, which typically requires root.** Run as a normal user it fails or prints a partial config. Mitigation: treat an unavailable `sshd -T` as an explicit UNKNOWN result with a hint, never as a PASS — a diagnostic that silently reports "fine" when it could not look is worse than one that admits it does not know.
- **`ssh -G` output is a stable but not contractual format.** Mitigation: parse leniently (match known keys, ignore everything else), and never fail the whole command because one line was unrecognised.
- **Scope creep toward "fix it for me".** The command reports; it does not edit anyone's ssh config. Writing to `~/.ssh/config` was explicitly ruled out in #97 and stays ruled out here.
