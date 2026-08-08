import { useEffect, useRef } from "react";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import type { TerminalBuffer } from "../types";

interface TerminalViewportProps {
  agentId: string;
  label: string;
  transcript: string;
  streamData?: TerminalBuffer;
  readOnly?: boolean;
  onInput: (data: string) => void;
  onResize: (cols: number, rows: number) => void;
  onFocus?: () => void;
}

export function TerminalViewport({
  agentId,
  label,
  transcript,
  streamData,
  readOnly,
  onInput,
  onResize,
  onFocus,
}: TerminalViewportProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | undefined>(undefined);
  const lastStreamRef = useRef<TerminalBuffer | undefined>(undefined);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const terminal = new Terminal({
      allowProposedApi: false,
      convertEol: false,
      cursorBlink: !readOnly,
      cursorStyle: "bar",
      disableStdin: readOnly,
      drawBoldTextInBrightColors: false,
      fontFamily: '"JetBrains Mono", "SFMono-Regular", Consolas, monospace',
      fontSize: 11.5,
      fontWeight: "400",
      fontWeightBold: "600",
      lineHeight: 1.28,
      scrollback: 4_000,
      theme: {
        background: "#141817",
        foreground: "#d8ddd8",
        cursor: "#5fc5b5",
        cursorAccent: "#141817",
        selectionBackground: "#3d5652",
        black: "#202524",
        red: "#e5746f",
        green: "#75b890",
        yellow: "#d6ae62",
        blue: "#7ca8bd",
        magenta: "#a89abb",
        cyan: "#65bcb0",
        white: "#d8ddd8",
        brightBlack: "#717a76",
        brightRed: "#f08b85",
        brightGreen: "#8ccc9f",
        brightYellow: "#e3c17b",
        brightBlue: "#91bfd2",
        brightMagenta: "#b9aacd",
        brightCyan: "#78cec1",
        brightWhite: "#f3f5f2",
      },
    });
    const fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    terminal.open(host);
    // GPU rendering. Without it xterm falls back to the DOM renderer, which
    // cannot keep up with several agents streaming output at once. Loading it
    // must happen after open(); a lost WebGL context degrades to the DOM
    // renderer rather than leaving a dead pane.
    let webglAddon: WebglAddon | undefined;
    try {
      webglAddon = new WebglAddon();
      webglAddon.onContextLoss(() => {
        webglAddon?.dispose();
        webglAddon = undefined;
      });
      terminal.loadAddon(webglAddon);
    } catch {
      webglAddon?.dispose();
      webglAddon = undefined;
    }
    terminalRef.current = terminal;
    terminal.write(transcript);

    const inputDisposable = terminal.onData((data) => {
      if (!readOnly) onInput(data);
    });
    const fit = () => {
      try {
        fitAddon.fit();
        if (terminal.cols > 0 && terminal.rows > 0) onResize(terminal.cols, terminal.rows);
      } catch {
        // A hidden/resizing pane can briefly have no measurable dimensions.
      }
    };
    const frame = window.requestAnimationFrame(fit);
    const observer = new ResizeObserver(fit);
    observer.observe(host);

    return () => {
      window.cancelAnimationFrame(frame);
      observer.disconnect();
      inputDisposable.dispose();
      webglAddon?.dispose();
      terminal.dispose();
      terminalRef.current = undefined;
      lastStreamRef.current = undefined;
    };
  }, [agentId, onInput, onResize, readOnly, transcript]);

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal || !streamData) return;
    const previous = lastStreamRef.current;
    if (!previous) {
      if (streamData.data.byteLength) terminal.write(streamData.data);
    } else if (
      streamData.generation === previous.generation
      && streamData.baseOffset === previous.baseOffset
      && streamData.data.byteLength > previous.data.byteLength
    ) {
      terminal.write(streamData.data.subarray(previous.data.byteLength));
    } else if (streamData !== previous) {
      terminal.reset();
      terminal.write(transcript);
      if (streamData.data.byteLength) terminal.write(streamData.data);
    }
    lastStreamRef.current = streamData;
  }, [streamData, transcript]);

  return (
    <div
      className="terminal-viewport"
      data-testid={`terminal-${agentId}`}
      onFocusCapture={onFocus}
      role="group"
      aria-label={`${label} terminal`}
    >
      <div ref={hostRef} className="terminal-host" />
    </div>
  );
}
