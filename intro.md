# Build `nah` — an output-aware keybind router for niri/biri (Rust)

## Mission

I run **biri** (a soft fork of niri that adds `main-axis "vertical"` scrolling
for portrait outputs). My keybinds for move / consume-or-expel do not
feel consistent across monitors: on horizontal-layout outputs the same keys
consume-or-expel windows left/right, and on vertical-layout outputs they effectively move
them up/down because the column strip is rotated. I want ONE set of keybinds
that always means "move the window one screen-slot to the left", regardless of
which monitor has focus.

Build a small Rust utility, `nah` (placeholder name, rename freely), consisting of:

1. `nah --daemon` — a long-lived daemon that:
   - holds a **persistent event-stream connection** to the compositor
   - initializes and updates a mapping of `workspace -> main-axis` by way of `workspace -> output -> logicaloutput -> transform -> main-axis`, where a transform of 90, 270, flipped 90, or flipped 270 indicates vertical main-axis and normal, 180, flipped normal, or flipped 180 indicate horizontal main-axis
   - keeps a live in-memory model of `window → workspace`
   - listens on a private UNIX socket,
   - receives short command lines ("intents") and translates each one into the
     correct compositor `Action` **based on the focused output's orientation**,
   - sends that Action over a **second** persistent IPC connection.
2. `nah <intent>` (default mode) — a tiny, fast client binary that connects to
   the daemon socket, sends one intent line, optionally waits ~50–100 ms for a
   reply, and exits. This gets spawned per keypress, so it must have a **minimal
   dependency tree and fast startup** (ideally std-only).
3. `nah status` — prints the daemon's current state: focused output name,
   configured main-axis for it, focused window id, and the resolved Action for each
   intent.
4. A systemd **user** unit, a README, and a KDL snippet.

## Verified protocol facts (build against these)

- The compositor exposes a UNIX stream socket at **`$NIRI_SOCKET`**; spawned
  processes inherit it. Fail loudly if it's unset.
- Protocol: newline-delimited JSON. A request is **one JSON object on a single
  line followed by `\n`**. Responses are `{"Handled":{}}` or `{"Err":"..."}`.
- `{"EventStream":{}}` starts a stream: the compositor sends
  **the complete current state up-front, then deltas**, and **stops reading any
  further requests on that connection**. Consequence: the daemon MUST hold
  **two separate connections** — one dedicated to the event stream, one for
  sending Actions. Never send an Action on the event-stream connection.
- Requests are processed one-by-one, in order, per connection.
- Events are externally-tagged JSON, e.g. `{"WorkspaceActivated":{"id":2,"focused":true}}` or `{"WorkspacesChanged":{"workspaces":[{"id":3,"idx":1,"name":null,"output":"HDMI-A-1","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":5,"is_hidden":false},{"id":6,"idx":2,"name":null,"output":"eDP-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null,"is_hidden":false},{"id":4,"idx":2,"name":null,"output":"DP-2","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null,"is_hidden":false},{"id":2,"idx":1,"name":null,"output":"eDP-1","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":16,"is_hidden":false},{"id":5,"idx":2,"name":null,"output":"HDMI-A-1","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null,"is_hidden":false},{"id":1,"idx":1,"name":null,"output":"DP-2","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":6,"is_hidden":false}]}}`.
- Actions are sent as `{"Action":{ ... }}`.

Relevant events to handle (verify exact field names against the installed
version's `niri-ipc` types or by capturing real events — see Phase 0):

| Event                | Daemon behavior                                     |
| -------------------- | --------------------------------------------------- |
| `WorkspacesChanged`  | replace whole workspace map                         |
| `WorkspaceActivated` | track focused workspace (fallback path)             |

## Facts you MUST verify, not assume

Do NOT hardcode assumptions about directional behavior. In particular:

- The biri docs claim directional focus/move shortcuts "keep their screen
  directions" in `main-axis "vertical"` mode, but my observed behavior says
  consume/expel move windows along the strip (up/down on screen) in that mode.
  These conflict — resolve it **empirically** (see Verification playbook) and
  encode the outcome **in config, not in code**.
- Upstream niri has actions like  `move-window-left/right`, `move-window-up/down`,
  `consume-or-expel-window-left/right`. Enumerate the real set available in the
  installed compositor with `niri msg action` (no args = full list), and confirm
  each Action's exact JSON encoding from the `niri-ipc` crate source before
  hardcoding it in defaults.

## Intent set & routing

Intents are **screen-relative**. Supported verbs × directions:

- `move-{left,right,up,down}`

Route each intent through: focused output → axis → per-(verb, direction) action
name. The table lives in config; code only falls back to sensible defaults.

Default mapping, `axis = "horizontal"` (standard niri semantics):

| Intent                 | Action                                                   |
| ---------------------- | -------------------------------------------------------- |
| move-left / move-right | `ConsumeOrExpelWindowLeft` / `ConsumeOrExpelWindowRight` |
| move-up / move-down    | `MoveWindowUp` / `MoveWindowDown`                        |
Default mapping, `axis = "vertical"`: 

| Intent                 | Action                                                   |
| ---------------------- | -------------------------------------------------------- |
| move-left / move-right | `MoveWindowLeft` / `MoveWindowRight`                     |
| move-up / move-down    | `ConsumeOrExpelWindowLeft` / `ConsumeOrExpelWindowRight` |
## Daemon socket (`$XDG_RUNTIME_DIR/nah.sock`)

- `SOCK_STREAM`, one intent per line, reply `ok\n` or `err <reason>\n`.
- Perms `0600`; verify peer UID via `SO_PEERCRED`; unlink stale socket on start.
- Concurrency: std threads + channels is sufficient — **no async runtime**.
- Never block on slow action delivery; drop with an `err` rather than queueing
  across reconnects.
- SIGTERM/SIGINT: unlink socket, exit 0.

## Phases — implement in this order, stopping to report after each

| Phase | Deliverable    | Acceptance criteria                                                                                                                                                                                                              |
| ----- | -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0     | Fixtures       | If you have a live session: capture `niri msg --json event-stream` output and real Action encodings; write them to `tests/fixtures/`. If not: implement against the documented schema and mark every field with `// UNVERIFIED`. |
| 1     | State tracker  | `nah --daemon` initializes the `workspace -> output` mapping, and logs the correct focused output name as I move focus across monitors (including empty workspaces and floating windows where `workspace_id` may be `None`).     |
| 2     | Action sender  | Second connection; sending a hardcoded action works end-to-end.                                                                                                                                                                  |
| 3     | Router         | Same `move-*` message produces different Actions on outputs with different axes; `nah status` reflects live state and resolved mappings.                                                                                         |
| 4     | KDL wiring     | Sample binds below work under key-repeat (hold the key 5 s: no visible lag, no process pile-up in `ps`).                                                                                                                         |
| 5     | Hardening      | systemd user unit survives compositor restart: daemon reconnects with backoff (200 ms → 5 s cap), resyncs state from the fresh full-state events, keeps serving.                                                                 |
| 6     | Tests + README | Unit tests for the state machine (fed from fixtures), pure-function tests for the router, config-parse tests; README documents all of the below.                                                                                 |

KDL wiring sample (for the README — note `spawn` runs the binary directly, no
shell, which is the point):

```kdl
binds {
    Mod+Shift+J { spawn "nah" "move-up"; }
    Mod+Shift+K { spawn "nah" "move-down"; }
    Mod+Shift+H { spawn "nah" "move-left"; }
    Mod+Shift+L { spawn "nah" "move-right"; }
}
```

systemd user unit must import the compositor env (e.g.
`systemctl --user import-environment NIRI_SOCKET`) or set it explicitly, since
services do not inherit the session env. (is this necessary if `niri`/`biri` spawns `nah` at startup?)

## Verification playbook (run in my session if available)

1. `niri msg action` → full action inventory; note the real names for
   the proposed nah-triggered actions.
2. `time (for i in $(seq 50); do nah move-left; done)` → expect ≪ 1 s total
   (~2–4 ms per press: fork/exec + one round trip). Report the number.

## Pitfalls / anti-goals

- **Never** send an Action over the event-stream connection.
- Do not treat `workspace_id` (window) or `output` (workspace) as non-optional;
  floating/unmapped windows and headless states exist. If the focused output is
  unknown, fall back to the plain horizontal default action rather than
  dropping the keypress.
- Do not shell out, do not read niri's `config.kdl`, do not poll (other than requesting output information in response to WorkspacesChanged) — the event
  stream is the only state source.
- No tokio/async, no heavy deps: daemon = `serde`, `serde_json`,
  `thiserror` (optional), `libc` or `rustix` for `SO_PEERCRED`. Client = std only.
- Client must never panic, never block longer than its timeout, and exit
  non-zero with a one-line stderr message on failure.
- Handle key-repeat volume: dozens of intents per second must be cheap
  (they are, if the hot path is: parse line → map lookup → one JSON line out).
- Every unverified assumption ends up either in config (preferred) or behind an
  `// UNVERIFIED:` comment — never silently hardcoded.

## Code quality

- Rust 2021+, `cargo clippy -- -D warnings` clean, `cargo fmt` clean.
- Split into a small workspace or one crate with a `client` and `daemon`
  feature; keep the client path dep-free.
- Unit + integration tests as specified in Phase 6; an integration test that
  runs a mock niri server on a temp socket (accepts `EventStream`, emits canned
  fixtures, records received Actions) must pass without a real compositor.
- MSRV: whatever the distro's stable Rust is; avoid bleeding-edge features.

## References

- IPC protocol & event stream: https://niri-wm.github.io/niri/IPC.html
- Actions overview: https://github.com/niri-wm/niri/wiki/Configuration:-Key-Bindings
- biri (vertical `main-axis` docs): https://github.com/barrulus/biri
- `niri_ipc` sub-crate documentation: https://docs.rs/niri-ipc/latest/niri_ipc/index.html
- Exact serde encodings: read the `niri-ipc` crate in the installed compositor's source tree.
