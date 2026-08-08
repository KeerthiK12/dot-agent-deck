export type RuntimeMode = "fixture" | "live";
export type ConnectionStatus = "loading" | "connected" | "disconnected" | "error";
export type RunHealth = "healthy" | "attention" | "failed" | "idle";
export type AgentStatus = "queued" | "running" | "waiting" | "passed" | "failed" | "stopped";
export type StageStatus = "queued" | "active" | "passed" | "failed" | "waiting";
export type PanelTab = "terminal" | "diff" | "checks" | "handoffs" | "artifacts";
export type Verdict = "PASS" | "FIX" | "HUMAN" | "ERROR" | "INFO";

export interface ConnectionView {
  status: ConnectionStatus;
  socketPath?: string;
  message?: string;
}

export interface WorkflowStage {
  id: string;
  label: string;
  agentId?: string;
  status: StageStatus;
  attempt: number;
  enabled: boolean;
}

export interface CheckResult {
  id: string;
  name: string;
  status: "passed" | "failed" | "running" | "queued";
  duration?: string;
  command?: string;
}

export interface Artifact {
  id: string;
  name: string;
  kind: "file" | "report" | "recording";
  path: string;
}

export interface AgentSession {
  id: string;
  paneId?: string;
  role: string;
  displayName: string;
  cli: string;
  model: string;
  status: AgentStatus;
  task: string;
  cwd: string;
  attempt: number;
  duration: string;
  tokens: number;
  cost: number;
  contextPercent: number;
  worktree: string;
  writeLease: "read" | "write" | "none" | "unknown";
  rows: number;
  cols: number;
  activeTool?: string;
  toolCount: number;
  transcript: string;
  diff: string[];
  checks: CheckResult[];
  handoffIds: string[];
  artifacts: Artifact[];
}

export interface EvidenceItem {
  id: string;
  verdict: Verdict;
  title: string;
  summary: string;
  from: string;
  to: string;
  at: string;
  command?: string;
  exitCode?: number;
  reason: string;
  acknowledged: boolean;
}

export type Provider = "OpenAI" | "Anthropic" | "OpenCode" | "Custom";
export type PermissionMode = "default" | "read-only" | "workspace-write" | "full-access";
export type ProfileCommandMode = "generated" | "custom";

export interface AgentProfile {
  id: string;
  roleId: string;
  role: string;
  provider: Provider;
  cli: string;
  model: string;
  effort: "low" | "medium" | "high" | "xhigh";
  commandMode: ProfileCommandMode;
  command: string;
  customCommand?: string;
  permissionMode: PermissionMode;
  enabled: boolean;
  savedToProject: boolean;
}

export interface DeckSnapshot {
  runId: string;
  repo: string;
  branch: string;
  worktree: string;
  connection: ConnectionView;
  health: RunHealth;
  elapsed: string;
  spend: number;
  currentNode: number;
  totalNodes: number;
  currentAttempt: number;
  paused: boolean;
  stages: WorkflowStage[];
  agents: AgentSession[];
  evidence: EvidenceItem[];
  profiles: AgentProfile[];
}

export type DeckAction =
  | { type: "pause_run" }
  | { type: "resume_run" }
  | { type: "approve_run" }
  | { type: "advance_fixture" }
  | { type: "start_daemon" }
  | { type: "start_workflow"; name: string; cwd: string; roles: WorkflowLaunchRole[]; rows: number; cols: number }
  | { type: "retry_stage"; stageId: string }
  | { type: "stop_agent"; agentId: string }
  | { type: "submit_text"; agentId: string; text: string };

export interface WorkflowLaunchRole {
  role: string;
  command: string;
  start: boolean;
}

export interface WorkflowLaunchConfig {
  name: string;
  cwd: string;
  roles: WorkflowLaunchRole[];
  rows: number;
  cols: number;
  customCommandCount: number;
  generatedFullAccessCount: number;
}

export interface TerminalChunk {
  agentId: string;
  data: Uint8Array;
  stream: "output" | "end" | "error";
  operation: "append" | "replace";
  generation?: number;
  message?: string;
}

export interface TerminalBuffer {
  data: Uint8Array;
  baseOffset: number;
  generation?: number;
}

export interface DeckRuntimeState {
  mode: RuntimeMode;
  snapshot: DeckSnapshot;
  terminalData: Record<string, TerminalBuffer>;
  error?: string;
  runAction: (action: DeckAction) => Promise<void>;
  sendTerminalInput: (agentId: string, data: string) => Promise<void>;
  resizeTerminal: (agentId: string, cols: number, rows: number) => Promise<void>;
  reconnect: () => Promise<void>;
}
