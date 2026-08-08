import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createFixtureSnapshot } from "./data/fixture";
import { WINDOWS_WORKFLOW_BLOCK_REASON } from "./lib/platform";
import type { DeckRuntimeState } from "./types";

vi.mock("./components/TerminalViewport", () => ({
  TerminalViewport: ({ agentId }: { agentId: string }) => <pre data-testid={`terminal-${agentId}`}>terminal</pre>,
}));

import { ControlDeck } from "./App";

function runtime(overrides: Partial<DeckRuntimeState> = {}): DeckRuntimeState {
  return {
    mode: "fixture",
    snapshot: createFixtureSnapshot("connected"),
    terminalData: {},
    runAction: vi.fn(async () => undefined),
    sendTerminalInput: vi.fn(async () => undefined),
    resizeTerminal: vi.fn(async () => undefined),
    reconnect: vi.fn(async () => undefined),
    ...overrides,
  };
}

describe("ControlDeck", () => {
  beforeEach(() => {
    window.localStorage.clear();
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: true,
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })));
  });

  afterEach(() => vi.unstubAllGlobals());

  it("renders the deterministic four-agent cockpit with evidence and stable test seams", () => {
    render(<ControlDeck runtime={runtime()} />);
    expect(screen.getByText("DEMO DATA")).toBeVisible();
    expect(screen.getByTestId("run-health")).toHaveTextContent("healthy");
    expect(screen.getByTestId("workflow-node-plan")).toBeVisible();
    expect(screen.getByTestId("agent-tile-builder")).toBeVisible();
    expect(screen.getByTestId("terminal-builder")).toBeVisible();
    expect(screen.getByTestId("evidence-drawer")).toBeVisible();
  });

  it("edits agent profiles locally and clearly marks the draft as not written to TOML", () => {
    render(<ControlDeck runtime={runtime()} />);
    fireEvent.click(screen.getByTestId("open-agent-profiles"));
    fireEvent.click(screen.getByRole("button", { name: /Coder/ }));
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "Anthropic" } });
    expect(screen.getByLabelText("CLI")).toHaveValue("claude");
    expect((screen.getByLabelText("Generated launch command") as HTMLTextAreaElement).value).toContain("'claude' --model");
    fireEvent.change(screen.getByLabelText("Provider"), { target: { value: "OpenAI" } });
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "gpt-5.6-sol-fast" } });
    expect((screen.getByLabelText("Generated launch command") as HTMLTextAreaElement).value).toContain("--model gpt-5.6-sol-fast");
    expect(screen.getByTestId("agent-profiles-panel")).toHaveTextContent("Local draft");
    expect(screen.getByRole("button", { name: /Coder/ })).toHaveTextContent("LOCAL");
    expect(window.localStorage.getItem("dot-agent-deck.desktop.agent-profiles.v1")).toContain("gpt-5.6-sol-fast");
  });

  it("launches the provider-derived command after CLI, model, effort, and permission edits", async () => {
    const live = runtime({ mode: "live" });
    render(<ControlDeck runtime={live} />);
    fireEvent.click(screen.getByTestId("open-agent-profiles"));
    fireEvent.click(screen.getByRole("button", { name: /Coder/ }));
    fireEvent.change(screen.getByLabelText("CLI"), { target: { value: "/Applications/Codex Nightly/bin/codex" } });
    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "gpt-5.6-sol-fast" } });
    fireEvent.change(screen.getByLabelText("Reasoning effort"), { target: { value: "high" } });
    fireEvent.change(screen.getByLabelText("Permission mode"), { target: { value: "full-access" } });
    fireEvent.click(screen.getByRole("button", { name: "Close agent profiles" }));

    fireEvent.click(screen.getByRole("button", { name: "Workflows" }));
    fireEvent.click(screen.getByTestId("launch-live-loop"));
    expect(screen.getByRole("alertdialog")).toHaveTextContent("All 6 commands are generated from the current provider, CLI, model, effort, and permission fields.");
    expect(screen.getByRole("alertdialog")).toHaveTextContent("Among generated commands, 1 role has full-access permissions.");
    fireEvent.click(screen.getAllByRole("button", { name: "Launch live loop" }).at(-1)!);

    await waitFor(() => {
      const launch = vi.mocked(live.runAction).mock.calls[0]?.[0];
      expect(launch).toMatchObject({ type: "start_workflow" });
      if (launch?.type !== "start_workflow") throw new Error("expected workflow launch");
      expect(launch.roles.find((role) => role.role === "coder")?.command).toBe("'/Applications/Codex Nightly/bin/codex' --model gpt-5.6-sol-fast --sandbox danger-full-access --ask-for-approval on-request -c model_reasoning_effort=high");
    });
  });

  it("requires an explicit advanced toggle for a custom command and calls out the bypass at launch", async () => {
    const live = runtime({ mode: "live" });
    render(<ControlDeck runtime={live} />);
    fireEvent.click(screen.getByTestId("open-agent-profiles"));
    fireEvent.click(screen.getByRole("button", { name: /Coder/ }));
    fireEvent.change(screen.getByLabelText("Permission mode"), { target: { value: "full-access" } });
    fireEvent.click(screen.getByLabelText("Use advanced custom command override"));
    fireEvent.change(screen.getByLabelText("Custom launch command"), { target: { value: "devbox run agent-coder" } });
    expect(screen.getByTestId("agent-profiles-panel")).toHaveTextContent("Permissions are unmanaged here and must be encoded and reviewed in that command.");
    fireEvent.click(screen.getByRole("button", { name: "Close agent profiles" }));

    fireEvent.click(screen.getByRole("button", { name: "Workflows" }));
    fireEvent.click(screen.getByTestId("launch-live-loop"));
    expect(screen.getByRole("alertdialog")).toHaveTextContent("1 explicit custom command override bypasses those fields");
    expect(screen.getByRole("alertdialog")).toHaveTextContent("Custom commands may carry arbitrary permissions and are not covered by structured permission claims.");
    expect(screen.getByRole("alertdialog")).not.toHaveTextContent("role has full-access permissions");
    fireEvent.click(screen.getAllByRole("button", { name: "Launch live loop" }).at(-1)!);

    await waitFor(() => {
      const launch = vi.mocked(live.runAction).mock.calls[0]?.[0];
      if (launch?.type !== "start_workflow") throw new Error("expected workflow launch");
      expect(launch.roles.find((role) => role.role === "coder")?.command).toBe("devbox run agent-coder");
    });
  });

  it("confirmation-gates live workflow launch and keeps orchestrator as the start role", async () => {
    const live = runtime({ mode: "live" });
    render(<ControlDeck runtime={live} />);
    fireEvent.click(screen.getByRole("button", { name: "Workflows" }));
    fireEvent.click(screen.getByTestId("launch-live-loop"));
    expect(live.runAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getAllByRole("button", { name: "Launch live loop" }).at(-1)!);
    await waitFor(() => expect(live.runAction).toHaveBeenCalledWith(expect.objectContaining({
      type: "start_workflow",
      name: "dot-agent-deck",
      roles: expect.arrayContaining([expect.objectContaining({ role: "orchestrator", start: true }), expect.objectContaining({ role: "coder", start: false })]),
    })));
  });

  it("explains and disables live workflow launch on Windows before confirmation", () => {
    const live = runtime({ mode: "live" });
    render(<ControlDeck runtime={live} workflowPlatformIssue={WINDOWS_WORKFLOW_BLOCK_REASON} />);
    fireEvent.click(screen.getByRole("button", { name: "Workflows" }));

    expect(screen.getByTestId("workflow-platform-issue")).toHaveTextContent("unavailable in this Windows preview");
    expect(screen.getByTestId("launch-live-loop")).toBeDisabled();
    fireEvent.click(screen.getByTestId("launch-live-loop"));
    expect(live.runAction).not.toHaveBeenCalled();
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
  });

  it("confirmation-gates an explicit daemon start from the disconnected state", async () => {
    const disconnected = createFixtureSnapshot("disconnected");
    disconnected.agents = [];
    let releaseStart!: () => void;
    const runAction = vi.fn(() => new Promise<void>((resolve) => { releaseStart = resolve; }));
    const live = runtime({ mode: "live", snapshot: disconnected, runAction });
    render(<ControlDeck runtime={live} />);
    fireEvent.click(screen.getByTestId("start-daemon"));
    expect(live.runAction).not.toHaveBeenCalled();
    fireEvent.click(screen.getAllByRole("button", { name: "Start daemon" }).at(-1)!);
    expect(screen.getByRole("button", { name: "Starting…" })).toBeDisabled();
    expect(screen.queryByText("Stopping…")).not.toBeInTheDocument();
    releaseStart();
    await waitFor(() => expect(live.runAction).toHaveBeenCalledWith({ type: "start_daemon" }));
    expect(live.reconnect).not.toHaveBeenCalled();
  });

  it("never reports daemon-start success when the start action fails", async () => {
    const disconnected = createFixtureSnapshot("disconnected");
    disconnected.agents = [];
    const live = runtime({
      mode: "live",
      snapshot: disconnected,
      runAction: vi.fn(async () => { throw new Error("daemon start timed out"); }),
    });
    render(<ControlDeck runtime={live} />);

    fireEvent.click(screen.getByTestId("start-daemon"));
    fireEvent.click(screen.getAllByRole("button", { name: "Start daemon" }).at(-1)!);

    await waitFor(() => expect(screen.getByText("daemon start timed out")).toBeVisible());
    expect(live.reconnect).not.toHaveBeenCalled();
    expect(screen.queryByText("Local daemon started and control channel reconnected.")).not.toBeInTheDocument();
  });

  it("keeps the mobile summary visible on first load and opens Evidence on demand", () => {
    vi.stubGlobal("matchMedia", vi.fn((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener: vi.fn(),
      removeListener: vi.fn(),
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(),
    })));

    render(<ControlDeck runtime={runtime()} />);
    expect(screen.queryByTestId("evidence-drawer")).not.toBeInTheDocument();
    expect(screen.getByTestId("agent-tile-planner")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop Planner" })).toBeVisible();

    fireEvent.click(screen.getByRole("button", { name: "Evidence" }));
    expect(screen.getByTestId("evidence-drawer")).toBeVisible();
  });
});
