---
sidebar_position: 6
title: Keyboard Shortcuts
---

# Keyboard Shortcuts

## Mouse

Every keyboard action below is also reachable with the mouse — the dashboard is fully clickable, not keyboard-only. Each clickable affordance carries its keyboard shortcut inline, so the on-screen controls double as a legend, and clicking one performs exactly the same action as its shortcut.

- **Persistent button bar.** The bottom row exposes the global commands — `[Back to Pane Ctrl+D]`, `[New Pane Ctrl+N]`, `[Close Ctrl+W]`, `[Toggle Layout Ctrl+T]`, `[Help ?]`, and `[Quit Ctrl+C]`. On terminals too narrow for the full labels the bar wraps to a second row rather than dropping any of them. This replaces the old status-bar legend. The bar follows the mode you are in: the first button reads `[Back to Pane Ctrl+D]` in command mode and `[Command Mode Ctrl+D]` while you are typing in a pane, and `[Close Ctrl+W]` is dimmed and inert outside command mode, matching the key. `[Close]` opens the same close confirmation the `Ctrl+W` key does. At the far left of the bar, before the buttons, a highlighted chip names the mode you are in right now — ` COMMAND ` or ` TYPING ` (see [Which mode you're in](#which-mode-youre-in)).
- **Tab strip.** Click a tab header to switch to it; Mode and Orchestration tabs carry a clickable `[×]` close affordance (the Dashboard tab has none). The `[×]` opens the same close confirmation as `Ctrl+W` and the `[Close]` button — every route into a pane teardown asks first.
- **Dashboard cards.** Single-click a card to select it, double-click to focus its pane. The bar adds clickable `[Filter /]`, `[Rename r]`, and `[Generate g]` buttons.
- **Scroll wheel.** A mode tab's side panes scroll when the pointer is over them; anywhere else the wheel scrolls the focused pane. The focused pane now scrolls in **command mode** too, not only while you are typing into it — command mode is a read-only inspect mode, so reading back through an agent's output no longer means entering the mode where a stray keystroke reaches it. In command mode the wheel always moves Agent Deck's own scrollback and is never forwarded to the agent's mouse protocol, so a full-screen TUI running in the pane cannot move under you while you read. While you are typing in a pane, the wheel is forwarded to the agent when it has mouse reporting enabled, exactly as before, and scrolls our scrollback otherwise. Side panes have always scrolled in any mode and are unchanged.
- **Dialogs, picker, and forms.** Each carries explicit clickable buttons alongside its keyboard controls: quit/config-gen/star/help dialog buttons; the directory picker's clickable rows, `..` parent, and `[Confirm]`/`[Cancel]`/`[Filter]`; the inline filter/rename `[Apply]`/`[Save]`/`[Cancel]`; the `[Command Mode Ctrl+D]` affordance while in a pane; and the new-pane form's clickable mode chips with `[Submit]`/`[Cancel]`.

All the keyboard shortcuts below continue to work unchanged.

## Global Shortcuts

| Key | Action | Works from |
|---|---|---|
| `Ctrl+D` | Toggle between command mode and the pane — press it in a pane to reach the dashboard, press it again to go back to the pane you came from | Any mode |
| `Ctrl+N` | New pane (directory picker, then name + command form) | Any mode |
| `Ctrl+T` | Toggle stacked / tiled layout | Any mode |
| `Ctrl+W` | Close the selected pane on the dashboard, or tear down the entire mode tab (agent + side panes) when used on a mode tab — after a confirmation dialog. The dashboard tab itself cannot be closed. | **Command mode only** |

### Which mode you're in

`Ctrl+D` toggles between two modes, and the deck names the one you are in rather than leaving you to infer it. A chip at the far left of the bottom bar reads ` COMMAND ` when your keystrokes drive the deck and ` TYPING ` when they go into the focused pane. It is in the same place on every tab, whenever the bar is showing its buttons — the one exception is while an inline **Filter** or **Rename** field is open, where that row *is* the input field and its own prompt tells you where your keystrokes are going. Those two words are the vocabulary the rest of this page uses: "command mode" is ` COMMAND `, and "typing in a pane" (`PaneInput` internally) is ` TYPING `.

The chip and the first button in the bar answer different questions, which is why both are there: the chip says where you *are*, while `[Back to Pane Ctrl+D]` / `[Command Mode Ctrl+D]` says where `Ctrl+D` would take you.

Three other things change with the mode:

- **No cursor in command mode.** The focused pane shows no cursor at all — neither the highlighted block nor your terminal's own blinking one. A cursor now means, without exception, that what you type lands in that pane.
- **The focused pane dims, with a banner.** Entering command mode dims the focused pane's content — still perfectly readable, just visibly inert — and overlays a large `COMMAND MODE · Ctrl+D to type` banner. The banner clears itself after about 2.5 seconds, or immediately when you press a command-mode key or click a bottom-bar button. Typing a key that isn't bound to anything keeps it up (or brings it back), because that is the moment you most likely thought you were talking to the agent. The dimming stays for as long as you are in command mode. In a pane too small for the block letters, the banner degrades to a single line and then drops out entirely; the chip and the dimming still tell you where you are.
- **The selected dashboard card dims while you type.** The selected card keeps its `▸ ` marker in both modes so you never lose track of the selection, but its highlight is de-emphasised while you are typing in a pane — the deck looks inert exactly when the pane looks live.

### `Ctrl+W` closes only from command mode

`Ctrl+W` is delete-previous-word in shells, readline, vim, and essentially every program you run inside a pane. So while you are typing in a pane, `Ctrl+W` is sent straight through to that program as `^W` (byte `0x17`) and deletes a word — it does not close anything. Press `Ctrl+D` first to reach command mode, and `Ctrl+W` there asks you to confirm before closing.

The confirmation defaults to **Cancel**, so an accidental `Ctrl+W` followed by a reflexive `Enter` leaves your pane exactly where it was. Choosing **Close** stops the agent and removes the card.

### `Ctrl+C`

In PaneInput mode, `Ctrl+C` is delivered to the terminal as SIGINT (0x03). From the dashboard (command mode), pressing `Ctrl+C` opens a quit confirmation dialog; press it again to quit immediately, or use the dialog keys (see [Dialogs](#dialogs)) to choose Yes / No.

## Tab Navigation

The tab bar appears when more than one tab is open.

| Key | Action |
|---|---|
| `Ctrl+PageDown` | Next tab (works from any mode, including in a focused pane) |
| `Ctrl+PageUp` | Previous tab (works from any mode, including in a focused pane) |
| `Tab` / `Right` / `l` | Next tab — **only in command mode** (press `Ctrl+D` first; otherwise the keystroke is sent to the agent pane) |
| `Shift+Tab` / `Left` / `h` | Previous tab — **only in command mode** (press `Ctrl+D` first; otherwise the keystroke is sent to the agent pane) |

## Mode Tab

These shortcuts work in Normal mode when a mode tab is active.

| Key | Action |
|---|---|
| `j` / `Down` | Focus next pane (cycles: agent → side panes → agent) |
| `k` / `Up` | Focus previous pane (cycles: agent → last side pane → … → agent) |
| `Enter` | Enter PaneInput mode on selected pane (agent pane if none selected) |
| `Esc` | Deselect side pane (return focus indicator to agent) |
| Mouse click | Click a side pane to select it; click agent pane to deselect |

In PaneInput mode, use `Ctrl+D` to return to Normal mode — and `Ctrl+D` again to go back into the pane.

## Dashboard

These shortcuts work in **command mode**. If you're typing in an agent pane, press `Ctrl+D` first to leave the pane — otherwise the keystroke is sent to the agent.

| Key | Action |
|---|---|
| `j` / `Down` | Select next card (wraps at end) |
| `k` / `Up` | Select previous card (wraps at start) |
| `1`–`9` | Jump to card N and focus its pane |
| `PageUp` | Scroll the focused pane back (see [Scrolling back through a pane](#scrolling-back-through-a-pane)) |
| `PageDown` | Scroll the focused pane forward |
| `/` | Filter sessions (opens filter input — see [Dialogs](#dialogs)) |
| `r` | Rename selected session (opens rename input — see [Dialogs](#dialogs)) |
| `g` | Generate `.dot-agent-deck.toml` (opens config-generation prompt — see [Dialogs](#dialogs)) |
| `s` | Open the **Scheduled Tasks** manager (`S` also works) (see [Scheduled Tasks](./scheduled-tasks.md)) |
| `?` | Toggle help overlay |
| `y` / `n` | Approve / deny a pending permission request (only when an agent is waiting) |
| `Esc` | Clear active filter |

### Scrolling back through a pane

`PageUp` / `PageDown` scroll the focused pane's output back and forward, three lines at a time — the keyboard equivalent of the scroll wheel. They are the `scroll_pane_up` and `scroll_pane_down` actions and are remappable like any other binding (see [Actions and defaults](#actions-and-defaults)).

They work in **command mode only**, and that is deliberate: while you are typing in a pane, `PageUp` and `PageDown` are sent straight through to whatever is running there as `ESC[5~` / `ESC[6~`, so a pager, an editor, or the agent's own scrollback keeps them. Press `Ctrl+D` first, and the same keys scroll the deck's own view of the pane instead.

These are the unmodified keys. `Ctrl+PageUp` / `Ctrl+PageDown` remain tab navigation and are unaffected.

## Directory Picker

| Key | Action |
|---|---|
| `j` / `Down` | Select next directory |
| `k` / `Up` | Select previous directory |
| `l` / `Right` / `Enter` | Enter directory (or confirm if no subdirs) |
| `h` / `Left` / `Backspace` | Go up one level |
| `Space` | Confirm current directory |
| `/` | Enter filter mode; type to narrow directories (case-insensitive) |
| `Esc` | Clear filter (press twice to close) |
| `q` | Cancel |

Directory lists loop end-to-end, so pressing `Up` on the first entry jumps to the last (and vice versa). The `..` parent entry always remains visible even when a filter is active.

## New Pane / Mode Form

| Key | Action |
|---|---|
| `Tab` / `Shift+Tab` | Switch between fields |
| `Left` / `Right` / `h` / `l` | Cycle mode selector (when modes available) |
| `Enter` | Confirm field / submit form |
| `Esc` | Cancel |

## Dialogs

Several dashboard shortcuts open transient input fields or selection dialogs. The keys for each:

| Dialog | Trigger | Keys |
|---|---|---|
| **Filter** | `/` | Type to narrow visible cards · `Backspace` to delete · `Enter` to accept and stay filtered · `Esc` to clear and close |
| **Rename** | `r` | Type the new name · `Enter` to confirm · `Esc` to cancel |
| **Generate config** | `g` | `Up`/`Down` (or `k`/`j`) to choose **Yes** / **No** / **Never** · `Enter` to confirm · `Esc` to cancel. **Yes** sends a prompt to the agent to write `.dot-agent-deck.toml`; **Never** suppresses the hint permanently for that directory. |
| **Quit confirmation** | `Ctrl+C` from command mode | `Up`/`Down` (or `k`/`j`) to choose **Yes** / **No** · `Enter` to confirm · `Esc` to dismiss · `Ctrl+C` again to quit immediately |
| **Close confirmation** | `Ctrl+W` from command mode, the `[Close]` button, or a tab's `[×]` | `Up`/`Down` (or `k`/`j`) to choose **Cancel** (default) / **Close** · `Enter` to confirm · `Esc` to dismiss. Only **Close** tears the pane (or tab) down, and it closes exactly what was selected when the dialog opened — the dialog holds the keyboard while it is up, and any keystroke you typed before it appeared is discarded rather than answering it. The dialog tells you which it is: `Close selected pane?` for a single dashboard pane, `Close this tab and all its panes?` whenever the target is a Mode or Orchestration tab — including a dashboard card whose pane happens to live in one. If any pane refuses to stop, the tab is kept holding whatever could not be closed, so you can press `Ctrl+W` again to retry. |
| **Help overlay** | `?` | `?`, `Esc`, or `q` to dismiss |

## Customizing Keybindings

Every shortcut above can be remapped. dot-agent-deck reads an optional config file at:

```
~/.config/dot-agent-deck/keybindings.toml
```

(Override the path with the `DOT_AGENT_DECK_KEYBINDINGS` environment variable.) Keybindings are resolved **client-side**, on the machine running the TUI — so when two clients attach to one remote daemon, each can have its own bindings.

The file has two sections, `[global]` and `[dashboard]`. You only need to list the actions you want to change; everything else keeps its default. The help overlay (`?`) and the hints bar are generated from the active config, so they always show your real keys.

### Key notation

- **Modifiers:** `Ctrl+`, `Alt+`, `Shift+` — combine in any order, e.g. `Alt+Shift+t`.
- **Named keys:** `Enter`, `Esc`, `Tab`, `Space`, `Up`, `Down`, `Left`, `Right`, `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Insert`, and `F1`–`F12`.
- **Printable characters:** `a`–`z`, `0`–`9`, `/`, `?`, etc.
- **Unbound:** an empty string (`new_pane = ""`) disables the action entirely.

Notation is case-insensitive for modifier and named keys (`ctrl+enter` == `Ctrl+Enter`).

### Example

```toml
# ~/.config/dot-agent-deck/keybindings.toml
# Only override what you need — defaults apply for everything else.

[global]
toggle_layout = "Alt+Shift+l"   # move it off Ctrl+t
new_pane = ""                    # disable the new-pane shortcut

[dashboard]
help = "F1"                      # open help with F1 instead of ?
```

### Actions and defaults

`[global]`:

| Action | Default | Description |
|---|---|---|
| `dashboard` | `Ctrl+d` | Toggle between command mode and the pane — works from any mode |
| `new_pane` | `Ctrl+n` | New pane (directory picker → name + command) — works from any mode |
| `close_pane` | `Ctrl+w` | Close selected pane / tear down mode tab, with confirmation — **command mode only**; in a pane the chord is ordinary input for whatever is running there |
| `toggle_layout` | `Ctrl+t` | Toggle stacked / tiled layout — works from any mode |
| `jump_1` … `jump_9` | `1` … `9` | Jump to card N and focus its pane |

`close_pane` stays in `[global]` — the section names the TOML table your binding is read from, not the modes it applies in, so an existing `[global] close_pane = "…"` line keeps working. Whatever chord you bind it to is command-mode only and reaches the pane as ordinary input everywhere else.

`[dashboard]` (command mode):

| Action | Default | Description |
|---|---|---|
| `move_down` | `j` | Select next card |
| `move_up` | `k` | Select previous card |
| `move_left` | `h` | Previous tab |
| `move_right` | `l` | Next tab |
| `filter` | `/` | Filter sessions |
| `rename` | `r` | Rename selected session |
| `help` | `?` | Toggle help overlay |
| `focus_pane` | `Enter` | Focus selected pane |
| `clear_filter` | `Esc` | Clear active filter |
| `approve_permission` | `y` | Approve a pending permission request |
| `deny_permission` | `n` | Deny a pending permission request |
| `generate_config` | `g` | Generate `.dot-agent-deck.toml` (config-generation prompt) |
| `scroll_pane_up` | `PageUp` | Scroll the focused pane back — **command mode only**; in a pane the key is passed to the agent |
| `scroll_pane_down` | `PageDown` | Scroll the focused pane forward — **command mode only**; in a pane the key is passed to the agent |

The `Down`/`Up`/`Tab`/`Shift+Tab`/`Left`/`Right` aliases and `Ctrl+PageUp` / `Ctrl+PageDown` tab navigation are not remappable and always work alongside your bindings. `Ctrl+PageUp` / `Ctrl+PageDown` are separate chords from the unmodified `PageUp` / `PageDown` above, so remapping the scroll actions does not affect tab navigation.

Rebinding `scroll_pane_up` / `scroll_pane_down` both enables the new chord and retires the default, so `scroll_pane_up = "Ctrl+u"` leaves plain `PageUp` doing nothing in command mode. Setting either to `""` unbinds it and leaves the scroll wheel as the only way to scroll that pane.

**Quit is not a remappable action.** No key directly quits — `Ctrl+C` (hardcoded, non-overridable) opens the quit/detach modal (Detach / Stop / Cancel). There is no `quit` config key; a `quit = "…"` line is treated as an unknown action and ignored with a warning.

### Edge cases

- **No config file** → all defaults (current behavior, nothing changes).
- **Malformed file** → dot-agent-deck warns on stderr and falls back to all defaults; it never crashes.
- **Conflicting bindings** (two actions on the same key) → a warning is printed and the first-defined action wins; the later one is left unbound.
- **Unknown action name** → ignored with a warning.
- **Empty binding** (`action = ""`) → that action is unbound and its default key does nothing.
- **`Ctrl+c` always quits.** It is a non-overridable safety net: quit is not a configurable action, and even if you bind another action to `Ctrl+c`, pressing `Ctrl+c` from command mode always opens the quit/detach modal — it is never routed through your config (so it can't be turned into "new pane", "switch tab", etc.).
