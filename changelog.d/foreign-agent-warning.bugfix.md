## An agent card that appears and vanishes now says where it came from

If a card for an agent you never started flickers onto the dashboard and disappears again, that is another deck's agent posting into your daemon — most often a test run, or a second checkout, whose child process inherited a `DOT_AGENT_DECK_SOCKET` pointing at your session. The card is registered because the hook arrives, then retired because no local pane backs it.

Until now this left nothing to go on: the daemon logged it as an ordinary `Received event`, indistinguishable from a real agent starting, so the only way to find out was to notice the flicker by eye and then read the log afterwards knowing exactly what to grep for. The daemon now logs a warning naming the pane, the session and the agent type when a `SessionStart` arrives for a pane it never spawned, along with the usual cause. Enable file logging with `DOT_AGENT_DECK_LOG=1` and search for `did not spawn`.

This is a warning rather than a refusal on purpose: a pane can legitimately belong to a client whose agent the daemon does not own, and dropping those hooks would break it. Nothing about which events are accepted has changed.
