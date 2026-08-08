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

  // The terminal is expensive to build (it allocates a GPU context) and owns
  // scroll position, selection, and cursor state. Anything that changes on
  // every snapshot — the growing transcript, or a callback identity — must be
  // reached through a ref instead of an effect dependency, or the pane is torn
  // down and rebuilt while the operator is typing in it.
  const transcriptRef = useRef(transcript);
  const onInputRef = useRef(onInput);
  const onResizeRef = useRef(onResize);
  transcriptRef.current = transcript;
  onInputRef.current = onInput;
  onResizeRef.current = onResize;

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
      fontSize: 13.5,
      fontWeight: "400",
      fontWeightBold: "600",
      lineHeight: 1.3,
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
    terminal.write(transcriptRef.current);

    const inputDisposable = terminal.onData((data) => {
      if (!readOnly) onInputRef.current(data);
    });
    const fit = () => {
      try {
        fitAddon.fit();
        if (terminal.cols > 0 && terminal.rows > 0) onResizeRef.current(terminal.cols, terminal.rows);
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
  }, [agentId, readOnly]);

  // A transcript that arrives (or is replaced) before the attach stream has
  // delivered anything still has to reach the screen — but by rewriting the
  // buffer, never by rebuilding the terminal. Once streaming owns the content,
  // the effect below is authoritative and this one stands down.
  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal || lastStreamRef.current) return;
    terminal.reset();
    terminal.write(transcript);
  }, [transcript]);

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
      terminal.write(transcriptRef.current);
      if (streamData.data.byteLength) terminal.write(streamData.data);
    }
    lastStreamRef.current = streamData;
  }, [streamData]);

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
