import { useEffect, useState } from "react";
import {
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  Bot,
  Check,
  GripVertical,
  RotateCcw,
  Save,
  SlidersHorizontal,
  X,
} from "lucide-react";
import { defaultCliForProvider, resolveProfileCommand } from "../lib/profileCommands";
import type { AgentProfile, Provider, RuntimeMode, WorkflowLaunchConfig } from "../types";

interface ProfilesPanelProps {
  open: boolean;
  profiles: AgentProfile[];
  onClose: () => void;
  onUpdate: (id: string, updates: Partial<AgentProfile>) => void;
  onReset: () => void;
  onSaved: () => void;
}

export function ProfilesPanel({ open, profiles, onClose, onUpdate, onReset, onSaved }: ProfilesPanelProps) {
  const [selectedId, setSelectedId] = useState(profiles[0]?.id);
  useEffect(() => {
    if (!profiles.some((profile) => profile.id === selectedId)) setSelectedId(profiles[0]?.id);
  }, [profiles, selectedId]);
  if (!open) return null;
  const profile = profiles.find((candidate) => candidate.id === selectedId);
  const commandResolution = profile ? resolveProfileCommand(profile) : undefined;

  return (
    <div className="sheet-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        className="config-sheet profiles-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="profiles-title"
        data-testid="agent-profiles-panel"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="sheet-header">
          <div>
            <span className="eyebrow">EXECUTION CONFIGURATION</span>
            <h2 id="profiles-title">Agent profiles</h2>
            <p>Generate a launch command from structured fields or explicitly opt into an unmanaged custom command.</p>
          </div>
          <button className="icon-button" aria-label="Close agent profiles" onClick={onClose}><X size={18} /></button>
        </header>

        <div className="local-only-notice">
          <AlertTriangle size={15} />
          <span><strong>Local draft</strong> — edits persist on this device but are not written to <code>.dot-agent-deck.toml</code> yet.</span>
        </div>

        <div className="profiles-layout">
          <nav className="profile-list" aria-label="Agent profiles">
            {profiles.map((item) => (
              <button key={item.id} className={item.id === selectedId ? "is-active" : ""} onClick={() => setSelectedId(item.id)}>
                <span className={`profile-enabled ${item.enabled ? "is-enabled" : ""}`}><Bot size={15} /></span>
                <span><strong>{item.role}</strong><small>{item.cli} · {item.model}</small></span>
                {!item.savedToProject && <em>LOCAL</em>}
              </button>
            ))}
          </nav>

          {profile ? (
            <form className="profile-form" onSubmit={(event) => { event.preventDefault(); onSaved(); }}>
              <div className="form-heading">
                <div><span>ROLE PROFILE</span><h3>{profile.role}</h3></div>
                <label className="switch-field">
                  <input type="checkbox" checked={profile.enabled} onChange={(event) => onUpdate(profile.id, { enabled: event.target.checked })} />
                  <span aria-hidden="true" />
                  Enabled in loop
                </label>
              </div>

              <div className="form-grid">
                <label>
                  <span>Role name</span>
                  <input value={profile.role} onChange={(event) => onUpdate(profile.id, { role: event.target.value })} />
                </label>
                <label>
                  <span>Provider</span>
                  <select value={profile.provider} onChange={(event) => {
                    const provider = event.target.value as Provider;
                    onUpdate(profile.id, {
                      provider,
                      cli: defaultCliForProvider(provider),
                      commandMode: provider === "Custom" ? "custom" : "generated",
                    });
                  }}>
                    <option>OpenAI</option><option>Anthropic</option><option>OpenCode</option><option>Custom</option>
                  </select>
                </label>
                <label>
                  <span>CLI</span>
                  <input value={profile.cli} spellCheck={false} onChange={(event) => onUpdate(profile.id, { cli: event.target.value })} />
                </label>
                <label>
                  <span>Model</span>
                  <input value={profile.model} spellCheck={false} onChange={(event) => onUpdate(profile.id, { model: event.target.value })} />
                </label>
                <label>
                  <span>Reasoning effort</span>
                  <select value={profile.effort} onChange={(event) => onUpdate(profile.id, { effort: event.target.value as AgentProfile["effort"] })}>
                    <option value="low">Low</option><option value="medium">Medium</option><option value="high">High</option><option value="xhigh">Extra high</option>
                  </select>
                </label>
                <label>
                  <span>Permission mode</span>
                  <select value={profile.permissionMode} onChange={(event) => onUpdate(profile.id, { permissionMode: event.target.value as AgentProfile["permissionMode"] })}>
                    <option value="default">CLI default</option><option value="read-only">Read only</option><option value="workspace-write">Workspace write</option><option value="full-access">Full access</option>
                  </select>
                </label>
                <div className="form-wide command-control">
                  <label className="command-override-toggle">
                    <input
                      type="checkbox"
                      checked={profile.commandMode === "custom"}
                      disabled={profile.provider === "Custom"}
                      onChange={(event) => onUpdate(profile.id, { commandMode: event.target.checked ? "custom" : "generated" })}
                    />
                    <span>Use advanced custom command override</span>
                  </label>
                  {profile.commandMode === "custom" ? (
                    <label>
                      <span>Custom launch command</span>
                      <textarea aria-label="Custom launch command" rows={3} value={profile.customCommand ?? ""} spellCheck={false} onChange={(event) => onUpdate(profile.id, { customCommand: event.target.value })} />
                      <small className="command-warning"><AlertTriangle size={11} /> Runs as an exact shell command and bypasses every provider field above. Permissions may be arbitrary and must be reviewed in this command. Never paste API keys or tokens here.</small>
                    </label>
                  ) : (
                    <label>
                      <span>Generated launch command</span>
                      <textarea aria-label="Generated launch command" rows={3} value={commandResolution?.command ?? ""} spellCheck={false} readOnly />
                      <small>Read-only preview. Provider, CLI, model, effort, and permission changes regenerate this command.</small>
                    </label>
                  )}
                  {commandResolution?.issue && <small className="command-error"><AlertTriangle size={11} /> {commandResolution.issue}</small>}
                  {commandResolution?.note && <small className="command-note">{commandResolution.note}</small>}
                </div>
              </div>

              <div className="profile-summary">
                <SlidersHorizontal size={15} />
                {commandResolution?.source === "custom"
                  ? <span><strong>{profile.role}</strong> will launch the exact custom command. Permissions are unmanaged here and must be encoded and reviewed in that command.</span>
                  : <span><strong>{profile.role}</strong> will launch the {profile.provider} command generated from these fields with {profile.permissionMode} permissions.</span>}
              </div>

              <footer className="sheet-footer">
                <button type="button" className="button secondary" onClick={onReset}><RotateCcw size={14} /> Reset defaults</button>
                <div>
                  <span>Auto-saved locally</span>
                  <button type="submit" className="button primary"><Save size={14} /> Confirm draft</button>
                </div>
              </footer>
            </form>
          ) : <div className="configuration-empty">No agent profile selected.</div>}
        </div>
      </section>
    </div>
  );
}

interface WorkflowPanelProps {
  open: boolean;
  profiles: AgentProfile[];
  order: string[];
  mode: RuntimeMode;
  defaultCwd: string;
  onClose: () => void;
  onToggle: (id: string) => void;
  onMove: (id: string, direction: -1 | 1) => void;
  onLaunch: (config: WorkflowLaunchConfig) => void;
  platformIssue?: string;
}

export function WorkflowPanel({ open, profiles, order, mode, defaultCwd, onClose, onToggle, onMove, onLaunch, platformIssue }: WorkflowPanelProps) {
  const [name, setName] = useState("dot-agent-deck");
  const [cwd, setCwd] = useState(defaultCwd.startsWith("/") ? defaultCwd : "");
  useEffect(() => {
    if (defaultCwd.startsWith("/") && (!cwd || cwd === "/dev/active/dot-agent-deck-gui")) setCwd(defaultCwd);
  }, [cwd, defaultCwd]);
  if (!open) return null;
  const ordered = [...order.map((id) => profiles.find((profile) => profile.id === id)).filter((profile): profile is AgentProfile => Boolean(profile)), ...profiles.filter((profile) => !order.includes(profile.id))];
  const enabled = ordered.filter((profile) => profile.enabled);
  const resolved = enabled.map((profile) => ({ profile, resolution: resolveProfileCommand(profile) }));
  const invalidCommands = resolved.filter(({ resolution }) => resolution.issue);
  const allRequiredRolesEnabled = enabled.length === ordered.length && enabled.some((profile) => profile.roleId === "orchestrator");
  const canLaunch = mode === "live" && !platformIssue && name.trim().length > 0 && cwd.startsWith("/") && allRequiredRolesEnabled && invalidCommands.length === 0;
  const roles = resolved.map(({ profile, resolution }) => ({ role: profile.roleId, command: resolution.command, start: profile.roleId === "orchestrator" }));
  const customCommandCount = resolved.filter(({ resolution }) => resolution.source === "custom").length;
  const generatedFullAccessCount = resolved.filter(({ profile, resolution }) => resolution.source === "generated" && profile.permissionMode === "full-access").length;
  return (
    <div className="sheet-backdrop" role="presentation" onMouseDown={onClose}>
      <section className="config-sheet workflow-sheet" role="dialog" aria-modal="true" aria-labelledby="workflow-editor-title" data-testid="workflow-editor" onMouseDown={(event) => event.stopPropagation()}>
        <header className="sheet-header">
          <div><span className="eyebrow">LOOP CONFIGURATION</span><h2 id="workflow-editor-title">Workflow order</h2><p>Shape the role sequence used by the cockpit preview.</p></div>
          <button className="icon-button" aria-label="Close workflow editor" onClick={onClose}><X size={18} /></button>
        </header>
        <div className="local-only-notice"><AlertTriangle size={15} /><span><strong>Local ordering</strong> — role order is a desktop draft. Launch uses these commands but does not rewrite project TOML.</span></div>
        {mode === "live" && (
          <div className="workflow-launch-form">
            <label><span>Workflow name</span><input value={name} onChange={(event) => setName(event.target.value)} placeholder="coding-loop" /></label>
            <label><span>Absolute project directory</span><input value={cwd} onChange={(event) => setCwd(event.target.value)} placeholder="/Users/you/dev/project" spellCheck={false} /></label>
            {cwd && !cwd.startsWith("/") && <small><AlertTriangle size={12} /> Use an absolute directory path.</small>}
            {platformIssue && <small data-testid="workflow-platform-issue"><AlertTriangle size={12} /> {platformIssue}</small>}
            {!allRequiredRolesEnabled && <small><AlertTriangle size={12} /> This project config requires orchestrator, coder, reviewer, auditor, tester, and release for a live launch.</small>}
            {invalidCommands.length > 0 && <small><AlertTriangle size={12} /> Fix the launch command for: {invalidCommands.map(({ profile }) => profile.role).join(", ")}.</small>}
          </div>
        )}
        <div className="workflow-editor-list">
          {ordered.map((profile, index) => (
            <div className={`workflow-editor-row ${profile.enabled ? "" : "is-disabled"}`} key={profile.id}>
              <GripVertical size={16} aria-hidden="true" />
              <span className="workflow-order">{String(index + 1).padStart(2, "0")}</span>
              <div><strong>{profile.role}{profile.enabled && profile.roleId === "orchestrator" ? <em className="start-role">START</em> : null}{profile.commandMode === "custom" ? <em className="custom-command-badge">CUSTOM CMD</em> : null}</strong><small><code>{profile.roleId}</code> · {profile.commandMode === "custom" ? "exact shell command · permissions unmanaged" : `${profile.cli} · ${profile.model}`}</small></div>
              <label className="compact-check"><input type="checkbox" checked={profile.enabled} onChange={() => onToggle(profile.id)} /><span>{profile.enabled ? <Check size={12} /> : null}</span><em>{profile.enabled ? "Enabled" : "Skipped"}</em></label>
              <div className="order-buttons">
                <button aria-label={`Move ${profile.role} up`} disabled={index === 0} onClick={() => onMove(profile.id, -1)}><ArrowUp size={14} /></button>
                <button aria-label={`Move ${profile.role} down`} disabled={index === ordered.length - 1} onClick={() => onMove(profile.id, 1)}><ArrowDown size={14} /></button>
              </div>
            </div>
          ))}
        </div>
        <footer className="sheet-footer workflow-footer">
          <span>{enabled.length} active roles · {ordered.length - enabled.length} skipped</span>
          {mode === "live" ? <button className="button primary" data-testid="launch-live-loop" disabled={!canLaunch} onClick={() => onLaunch({ name: name.trim(), cwd, roles, rows: 32, cols: 120, customCommandCount, generatedFullAccessCount })}><Bot size={14} /> Launch live loop</button> : <button className="button primary" onClick={onClose}><Check size={14} /> Use preview</button>}
        </footer>
      </section>
    </div>
  );
}
