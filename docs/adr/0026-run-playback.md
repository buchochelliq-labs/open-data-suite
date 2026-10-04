# ADR-0026: Run playback: replaying a run's journal on the Lineage page

- **Status:** Accepted (2026-10-04)
- **Date:** 2026-10-04
- **Issues:** follow-up to #322 (run events, journal and live view)
- **Deciders:** @n1ckyb

## Context
The live run view (#322, [ADR-0024](0024-run-events-node-stats-and-run-journal.md))
shows a run as it goes: each node queued, running, built or failed, follow mode keeping
the running nodes in view. Once the run is over, all that is left is its final state.
To debug a run ("what was running when `orders` failed?", "why did the downstream
start so late?") or to analyse its performance ("how parallel was it?", "which node
held everything up?"), people need to see the run *again*, at their own pace: at normal
speed, paused, faster or slower, and by dragging a play bar to any moment, as on a
video.

The journal already has what that needs: every event of the run, in order, each with
its time (`at`), kept beside the state database (`<state-db>.runs/<run_id>.jsonl`, the
newest 50 runs). `ods serve` already serves it (`/api/runs/<run_id>/events`, and
`?since=<n>` as JSON lines), redacted and with checks by their handles (ADR-0024,
ADR-0025). And the live view already folds events into node states with a pure function
(`OdsLive.apply`, the same fold as `RunSummary::from_events`).

Constraints: read-only (no route may start or change a run); no secrets or values
(rule 9: only what the stream already sends); conservative (rule 3: a time the journal
doesn't hold is never invented); presentation only (rule 7: no new logic in core).

## Options considered
### Option A: a video or recording of the page
Pros: plays anywhere. Cons: must be recorded while the run goes (most runs aren't
watched); fixed to one camera; heavy; nothing to select or inspect.

### Option B: server-side playback (the server streams the journal again, paced)
Pros: the client stays as it is. Cons: one paced stream per viewer and per seek; seeking
backwards means reconnecting; pause and speed need a control channel; the server would
hold a timer per viewer for something the browser can do alone.

### Option C: client-side playback of the journal (chosen)
The page loads the whole journal once through the existing route and replays it
itself: the state at any moment is the fold of the events up to that moment. Pros: no
new route or format; seek, pause and speed are local and instant; the same drawing,
follow mode, node cards and explanations as the live view; works for any journal ODS
still keeps. Cons: the journal is held in the page (bounded: ADR-0024's limits, and a
journal of a run of thousands of nodes is a few MB); a run still going can only be
replayed up to the moment it was loaded.

## Decision
### What is replayed
The run's journal, loaded with `GET /api/runs/<run_id>/events?since=<n>` (JSON lines,
2,000 at a time) until the answer is empty or says `end`: the same sanitized events the
live stream sends, so playback can show nothing the stream can't. The run's **timeline**
runs from its `run_started` to its `run_finished` (or its last event, for a run that
didn't finish); its length is the run's duration.

The state at a moment `t` is the fold (`OdsLive.apply`) of every event whose `at` is
at or before `t`, in journal order: the live view's own fold, so a replayed run looks
exactly as it did live at that moment, and the end of the replay is the run's final
state. Seeking is deterministic: the same `t` always gives the same picture. To seek
fast in long journals, the page keeps a **keyframe** (a copy of the folded state) every
256 events and folds forward from the nearest one; playing forward applies events
incrementally.

A **running** node's elapsed time is measured from the playhead, not the wall clock:
the view's clock is the playhead in playback and `Date.now()` live, one function both
use. A node's **stats card** never shows its future: until the playhead passes the
node's last event, the card is built from the state at the playhead (status, start,
time, rows so far, and a note saying so); the Run page's card (the run's final word on
the node, its explanation included) shows only once the playhead is past it.

### Controls
A play bar along the bottom of the Lineage canvas, as a video player has:
- **Play / pause** (Space or K); at the end, Play starts again from the beginning.
- **Speed**: 0.25×, 0.5×, 1× (the run's real pace), 2×, 4×, 8×, 16×, 64× (Shift+`<` /
  Shift+`>`, or the speed button).
- **Scrub**: a slider over the whole run; dragging it moves the playhead (and pauses
  while dragging); ← / → move 5 s, J / L 10 s, Home / End to either end, 0–9 to 0–90 %.
- **Step**: the previous or next **event** (`,` / `.`), not a fixed time: what
  debugging needs is "what happened next", whatever the gap.
- The **time**, `m:ss / m:ss` (elapsed since the run started / its duration), and the
  wall-clock time at the playhead in its title.

Above the slider, the timeline shows what the run did, so a moment can be found by
eye: a **concurrency band** (how many nodes were running at each moment, scaled to the
run's peak: gaps and narrow stretches are where the run was waiting on one node) and
**markers** for each failure (red) and the run's end. Hovering or focusing a marker
names it; activating it seeks there. This is the performance view: the band shows
parallelism over the run, and the node cards (already in the live view) show each
node's time, rows and thread.

Follow mode works while playing, as live: the camera keeps the running nodes in view
(and is turned off by panning, as live). Seeking redraws every node at once, without
announcing each event; screen readers hear the play state and the time when it
changes, never a flood of events.

### Where it opens
- `/lineage?replay=<run_id>`, optionally `&t=<seconds>` (the playhead) and
  `&speed=<n>`: a link to a moment of a run, to share when debugging. The URL follows
  the playhead when paused or seeked (not every frame while playing).
- A **Replay** button in the live view's toast once a run has finished, and on the Run
  page (`/state/runs/<run_id>`) for a run whose journal ODS still keeps.

### Conservative where the journal is thin (rule 3)
An event whose time is missing, unreadable or earlier than the event before it (a
clock that went back) is played at the time of the event before it, and counted: the
play bar says how many times are inferred, and a marker at such a time reads "at
about" and is drawn dashed. A journal that isn't there (`404`) is said to be missing;
one that couldn't be loaded (e.g. `503`, a dropped connection) says so and offers
*Try again*; one whose lines can't be read says how many; one with no events yet says
the run may be starting and offers *Go live*.

A journal rebuilt after the run from its final report (`live: false`, ADR-0024) has no
start events and only the times the engine reported: nodes are shown going from queued
straight to their outcome, at the time each finished, and the play bar says "Times from
the run's final report: when each node finished, not when it started." A run still
going is replayed up to the moment it was loaded, and the bar offers **Go live**
(switching to the live view); its end is never shown as a finish. Lines that can't be
read are counted, as live, never drawn.

### What it doesn't do
Nothing is stored and no route is added: playback is computed in the page from the
journal (ADR-0024: computed on read). It needs `ods serve`; a static export has no
journals, so it offers no playback. Journals pruned from the newest 50 can't be
replayed, and the page says so.

## Consequences
- Positive:
  - Any run ODS keeps can be watched again, at any pace, from any moment; a link names
    a moment, for a colleague or an issue.
  - The concurrency band makes a run's parallelism, and its waits, visible at a glance.
  - One fold, one drawing: live and replay can't drift apart.
- Negative / trade-offs:
  - The page holds the whole journal (a few MB for thousands of nodes).
  - Playback is as precise as the journal's times (milliseconds) and as rich as its
    events: no per-statement detail within a node.
  - Runs rebuilt from a final report replay coarsely, and say so.
- Follow-up:
  - A Gantt view of the same timeline (one row per thread) for performance analysis.
  - Comparing two runs' timelines side by side.
  - Replay in the VS Code extension (M5), from the same route.

## References
- #322; [ADR-0024](0024-run-events-node-stats-and-run-journal.md) (events, journal and
  live stream), [ADR-0025](0025-error-explanations.md) (explanations shown in node cards),
  [ADR-0009](0009-hostable-explorer-ods-web.md) (`ods-web`).
- `crates/ods-web/assets/live.js` (`OdsLive`: the fold, the timeline, seeking).
