import type { EvidenceItem, Verdict } from "../types";

/**
 * Cap on the in-memory live evidence ring. Hook events arrive for every tool
 * call, so an uncapped list grows without bound over a long run.
 */
export const MAX_LIVE_EVIDENCE = 500;

/**
 * The `desktop://daemon-event` payload is the daemon's `BroadcastMsg`, an
 * internally tagged enum (`kind`). Its `event` variant flattens an `AgentEvent`,
 * whose fields are serialized in **snake_case** — the Rust struct carries no
 * camelCase rename, unlike the desktop DTOs.
 */
export interface DaemonHookEvent {
  kind?: string;
  session_id?: unknown;
  agent_type?: unknown;
  event_type?: unknown;
  tool_name?: unknown;
  tool_detail?: unknown;
  cwd?: unknown;
  timestamp?: unknown;
  user_prompt?: unknown;
  pane_id?: unknown;
  agent_id?: unknown;
}

/** Resolves the deck-side agent behind a hook event, when one is known. */
export type AgentResolver = (agentId?: string, paneId?: string) => { id: string; role: string } | undefined;

const EVENT_TITLES: Record<string, string> = {
  session_start: "Session started",
  session_end: "Session ended",
  tool_start: "Tool started",
  tool_end: "Tool finished",
  thinking: "Thinking",
  compacting: "Compacting context",
  subagent_start: "Subagent started",
  subagent_stop: "Subagent finished",
  waiting_for_input: "Waiting for input",
  permission_request: "Permission requested",
  idle: "Idle",
  error: "Agent reported an error",
};

const HUMAN_EVENTS = new Set(["waiting_for_input", "permission_request"]);

function text(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function verdictFor(eventType: string): Verdict {
  if (eventType === "error") return "ERROR";
  if (HUMAN_EVENTS.has(eventType)) return "HUMAN";
  return "INFO";
}

function clockFor(timestamp: unknown): string {
  const raw = text(timestamp);
  if (!raw) return "—";
  const parsed = new Date(raw);
  if (Number.isNaN(parsed.getTime())) return "—";
  return parsed.toLocaleTimeString([], { hour12: false, hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function summaryFor(event: DaemonHookEvent, eventType: string): string {
  const tool = text(event.tool_name);
  const detail = text(event.tool_detail);
  if (tool) return detail ? `${tool} · ${detail}` : tool;
  const prompt = text(event.user_prompt);
  if (prompt) return prompt.length > 240 ? `${prompt.slice(0, 240)}…` : prompt;
  const agentType = text(event.agent_type)?.replaceAll("_", " ");
  return agentType ? `${agentType} reported ${eventType.replaceAll("_", " ")}.` : `Agent reported ${eventType.replaceAll("_", " ")}.`;
}

/**
 * Maps one `desktop://daemon-event` payload onto the evidence model the drawer
 * already renders, or returns `undefined` for anything this build should ignore
 * — the `orchestration_surface` variant, an unrecognised `kind`, or an
 * `event_type` a future daemon adds. Ignoring beats guessing: a mis-mapped
 * event would show the operator an edge that never happened.
 *
 * Hook events are point-in-time signals, not handoff edges, so `to` stays empty
 * rather than inventing a receiver the daemon never reported.
 */
export function mapDaemonEvent(payload: unknown, sequence: number, resolveAgent?: AgentResolver): EvidenceItem | undefined {
  if (!payload || typeof payload !== "object") return undefined;
  const event = payload as DaemonHookEvent;
  if (event.kind !== undefined && event.kind !== "event") return undefined;
  const eventType = text(event.event_type);
  if (!eventType || !(eventType in EVENT_TITLES)) return undefined;

  const agentId = text(event.agent_id);
  const paneId = text(event.pane_id);
  const agent = resolveAgent?.(agentId, paneId);
  const sessionId = text(event.session_id);

  return {
    id: `hook-${sequence}`,
    verdict: verdictFor(eventType),
    title: EVENT_TITLES[eventType],
    summary: summaryFor(event, eventType),
    from: agent?.role ?? agentId ?? paneId ?? sessionId ?? "Unattributed agent",
    to: "",
    at: clockFor(event.timestamp),
    reason: "Live hook event from the daemon event stream. Delegate and work-done handoff edges are not yet reported (PRD #176 M3.1).",
    acknowledged: false,
    agentId: agent?.id ?? agentId,
  };
}
