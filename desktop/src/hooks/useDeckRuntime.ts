import { useCallback, useEffect, useMemo, useState } from "react";
import { createFixtureSnapshot } from "../data/fixture";
import { createDeckBridge, selectRuntimeMode } from "../lib/bridge";
import { applyTerminalChunk } from "../lib/terminalBuffer";
import type { DeckAction, DeckRuntimeState, DeckSnapshot, TerminalBuffer } from "../types";

export function useDeckRuntime(): DeckRuntimeState {
  const mode = useMemo(selectRuntimeMode, []);
  const bridge = useMemo(() => createDeckBridge(mode), [mode]);
  const [snapshot, setSnapshot] = useState<DeckSnapshot>(() => {
    const initial = createFixtureSnapshot("empty");
    return {
      ...initial,
      worktree: mode === "live" ? "No active project" : initial.worktree,
      connection: { status: "loading", message: mode === "live" ? "Connecting to local daemon…" : "Loading deterministic fixture…" },
    };
  });
  const [error, setError] = useState<string>();
  const [terminalData, setTerminalData] = useState<Record<string, TerminalBuffer>>({});

  const updateTerminal = useCallback((event: Parameters<typeof applyTerminalChunk>[1]) => {
    const data = event.data.byteLength
      ? event.data
      : event.message
        ? new TextEncoder().encode(`\r\n[terminal] ${event.message}\r\n`)
        : event.data;
    setTerminalData((current) => {
      const next = applyTerminalChunk(current[event.agentId], { ...event, data });
      if (next === current[event.agentId]) return current;
      return { ...current, [event.agentId]: next };
    });
  }, []);

  const reconnect = useCallback(async () => {
    setError(undefined);
    setSnapshot((current) => ({ ...current, connection: { ...current.connection, status: "loading", message: "Reconnecting…" } }));
    try {
      const connected = await bridge.connect();
      setSnapshot(connected);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      setSnapshot((current) => ({
        ...current,
        health: "failed",
        connection: { status: "error", message },
      }));
    }
  }, [bridge]);

  useEffect(() => {
    let active = true;
    let unsubscribe: (() => void) | undefined;

    void (async () => {
      try {
        unsubscribe = await bridge.subscribe(
          (next) => active && setSnapshot(next),
          (event) => active && updateTerminal(event),
        );
        if (!active) {
          unsubscribe();
          return;
        }
        const initial = await bridge.connect();
        if (active) setSnapshot(initial);
      } catch (cause) {
        if (!active) return;
        const message = cause instanceof Error ? cause.message : String(cause);
        setError(message);
        setSnapshot((current) => ({ ...current, health: "failed", connection: { status: "error", message } }));
      }
    })();

    return () => {
      active = false;
      unsubscribe?.();
      void bridge.dispose();
    };
  }, [bridge, updateTerminal]);

  const runAction = useCallback(async (action: DeckAction) => {
    setError(undefined);
    try {
      return await bridge.runAction(action);
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      throw cause;
    }
  }, [bridge]);

  const sendTerminalInput = useCallback((agentId: string, data: string) => bridge.sendTerminalInput(agentId, data), [bridge]);
  const resizeTerminal = useCallback((agentId: string, cols: number, rows: number) => bridge.resizeTerminal(agentId, cols, rows), [bridge]);

  return {
    mode,
    snapshot,
    terminalData,
    error,
    runAction,
    sendTerminalInput,
    resizeTerminal,
    reconnect,
  };
}
