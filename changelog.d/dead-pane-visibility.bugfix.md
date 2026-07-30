## A pane that loses its agent now says so, instead of going quietly dead

When a pane's agent went away and the deck's reconnect attempts ran out, the pane kept rendering its last frame and looked completely healthy — but every keystroke was silently dropped. The only hint was a status message that flashed `PTY write failed: Pane <id> stream I/O task ended`, naming an internal task rather than telling you the agent was gone. Selecting such a pane and typing looked, from the outside, like the deck had frozen.

Those panes are now labelled `— disconnected` in the title, so the state is visible before you type anything, and their last output is preserved so you can still read what the agent did before it went away. Typing into one now reports what actually happened and what you can do about it — "Agent is no longer running — pane is disconnected. Close it to start over." — instead of an internal error. Nothing is closed automatically: the pane stays until you close it.

The two situations that lead here are reported distinctly, because their causes are unrelated: an agent that exits on every restart attempt (usually the agent's own command failing at startup) versus an agent the daemon no longer has at all (stopped deliberately, or a daemon restart underneath the pane).

Both give-up paths now log at warning level and name which one was taken, so a report of "my pane died" can actually be diagnosed. Previously they logged at debug level, which meant that unless you had already set `DOT_AGENT_DECK_LOG`, four different causes produced one identical symptom and left no evidence behind. See [Troubleshooting › A pane says "disconnected" and ignores what you type](https://agent-deck.devopstoolkit.ai/docs/troubleshooting) for how to capture that detail when filing an issue.
