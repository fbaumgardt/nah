# niri IPC serde wire encodings — pinned protocol reference

Phase 0 protocol-pinning notes for project `nah`. Every fact below is tagged **[V]** (verified
against a primary source) or **[UNVERIFIED]**. Retrieval dates: docs.rs pages read 2026-09-23;
released-crate source and GitHub raw sources fetched 2026-09-28.

The wire format is **serde's default externally-tagged enum representation** for every
Request/Response/Action/Event enum — no `#[serde(rename_all)]`, `tag`, `content`, or
`deny_unknown_fields` attributes exist on any of these types. Verified directly in the released
crate source (niri-ipc **26.4.0**, per docs.rs `latest`; the crate doc comment itself instructs
pinning `niri-ipc = "=26.4.0"`):

- Source with attributes (authoritative for serde encodings):
  https://docs.rs/niri-ipc/latest/src/niri_ipc/lib.rs.html
  (defs at lines: `Request` 65–67, `Reply` 132, `Response` 135–137, `Action` 192–194,
  `Transform` 1266–1269, `Window` 1295–1297, `Workspace` 1406–1408, `Event` 1569–1571)
- serde enum-representation reference: https://serde.rs/enum-representations.html (externally
  tagged is the default; struct variants serialize as `{"Variant": {…}}`)
- **[V]** Additional empirical check: compiled the exact derive set (serde 1.0.228,
  serde_json 1.0.150) locally and confirmed the shapes quoted below (2026-09-28).

---

## 1. Request envelope

**[V]** `Request` carries only `#[derive(Debug, Serialize, Deserialize, Clone)]` (+ cfg_attr
json-schema/clap). No serde attributes → externally tagged, PascalCase variant names exactly as
written in Rust.

| Rust variant | JSON on the wire |
|---|---|
| `Request::Action(Action::MoveWindowUp {})` | `{"Action":{"MoveWindowUp":{}}}` |
| `Request::Action(Action::ConsumeOrExpelWindowLeft { id: Some(5) })` | `{"Action":{"ConsumeOrExpelWindowLeft":{"id":5}}}` |
| `Request::Action(Action::ConsumeOrExpelWindowLeft { id: None })` | `{"Action":{"ConsumeOrExpelWindowLeft":{"id":null}}}` |
| `Request::EventStream` | `"EventStream"` — **a bare JSON string, NOT `{"EventStream":{}}`** |
| `Request::Version` / `Outputs` / `Workspaces` / … (any unit variant) | `"Version"`, `"Outputs"`, … |
| `Request::Output { output: String, action: OutputAction }` | `{"Output":{"output":"DP-1","action":{…}}}` |

- Unit variants serialize as the variant-name string; fieldless struct variants serialize as
  `{"Name":{}}`. Both behaviors confirmed empirically with serde 1.0.228/serde_json 1.0.150
  (`"EventStream"`, `{"Action":{"MoveWindowUp":{}}}`) — see serde reference above for the
  externally-tagged default.
- `Option<T>` `None` serializes as JSON `null`; a *missing* `Option` field also deserializes as
  `None` (verified empirically).
- Full variant list (niri-ipc 26.4.0, lib.rs 67–122 / https://docs.rs/niri-ipc/latest/niri_ipc/enum.Request.html):
  `Version`, `Outputs`, `Workspaces`, `Windows`, `Layers`, `KeyboardLayouts`, `FocusedOutput`,
  `FocusedWindow`, `PickWindow`, `PickColor`, `Action(Action)`,
  `Output { output: String, action: OutputAction }`, `EventStream`, `ReturnError`,
  `OverviewState`, `Casts` — 16 variants.

## 2. Response envelope

**[V]** `pub type Reply = Result<Response, String>;` (lib.rs 132;
https://docs.rs/niri-ipc/latest/niri_ipc/type.Reply.html). `Result` serializes with externally
tagged `Ok`/`Err` variants (no serde attributes exist or are possible on a type alias):

- Success, no payload (all actions): `{"Ok":"Handled"}` — `Response::Handled` is a *unit*
  variant, so it is the string `"Handled"`, **not `{"Handled":{}}`** (verified empirically).
- Success with payload: `{"Ok":{"Version":"26.4"}}`, `{"Ok":{"Outputs":{…}}}`,
  `{"Ok":{"Workspaces":[…]}}` etc.
- Error: `{"Err":"example compositor error"}` (string payload; server emits exactly this for
  `Request::ReturnError` — niri `src/ipc/server.rs`
  https://raw.githubusercontent.com/YaLTeR/niri/main/src/ipc/server.rs, `process()` first arm).
- `Response` variants (26.4.0, lib.rs 137–168 / https://docs.rs/niri-ipc/latest/niri_ipc/enum.Response.html):
  `Handled`, `Version(String)`, `Outputs(HashMap<String, Output>)`, `Workspaces(Vec<Workspace>)`,
  `Windows(Vec<Window>)`, `Layers(Vec<LayerSurface>)`, `KeyboardLayouts(KeyboardLayouts)`,
  `FocusedOutput(Option<Output>)`, `FocusedWindow(Option<Window>)`,
  `PickedWindow(Option<Window>)`, `PickedColor(Option<PickedColor>)`,
  `OutputConfigChanged(OutputConfigChanged)`, `OverviewState(Overview)`, `Casts(Vec<Cast>)`.
  Note the past-tense names `PickedWindow`/`PickedColor` (requests are `PickWindow`/`PickColor`).
- The event-stream request also gets **one reply first**: `{"Ok":"Handled"}` — server.rs has
  `Request::EventStream => Response::Handled` in `process()`, and `handle_client` writes the
  reply before switching the connection to event mode (server.rs, `handle_client` /
  `requested_event_stream` branch). Same statement in the fork's doc comment (biri lib.rs,
  `Request::EventStream` docs: "The compositor should reply with `Reply::Ok(Response::Handled)`,
  then continuously send `Event`s, one per line.").
- Protocol doc ("reply is an `Ok` or an `Err` wrapping the same JSON object as you get from
  `niri msg --json`"): https://niri-wm.github.io/niri/IPC.html

## 3. Action variants (window move / consume-or-expel) — upstream vs biri fork

**[V]** Upstream niri-ipc 26.4.0 has **no `MoveWindowLeft` / `MoveWindowRight`**. Horizontal
movement is *column*-level: `MoveColumnLeft {}`, `MoveColumnRight {}`, `MoveColumnToFirst {}`,
`MoveColumnToLast {}`, `MoveColumnLeftOrToMonitorLeft {}`, `MoveColumnRightOrToMonitorRight {}`,
`MoveColumnToIndex { index: usize }` (full 141-variant list:
https://docs.rs/niri-ipc/latest/niri_ipc/enum.Action.html).

**[V]** Upstream vertical within-column move + consume/expel variants (exact serde names,
lib.rs `Action`):

| Rust variant | Payload | JSON |
|---|---|---|
| `MoveWindowUp {}` | — | `{"Action":{"MoveWindowUp":{}}}` |
| `MoveWindowDown {}` | — | `{"Action":{"MoveWindowDown":{}}}` |
| `MoveWindowUpOrToWorkspaceUp {}` | — | `{"Action":{"MoveWindowUpOrToWorkspaceUp":{}}}` |
| `MoveWindowDownOrToWorkspaceDown {}` | — | `{"Action":{"MoveWindowDownOrToWorkspaceDown":{}}}` |
| `ConsumeOrExpelWindowLeft { id: Option<u64> }` | optional window id | `{"Action":{"ConsumeOrExpelWindowLeft":{"id":123}}}` |
| `ConsumeOrExpelWindowRight { id: Option<u64> }` | optional window id | `{"Action":{"ConsumeOrExpelWindowRight":{}}}` (id omitted/None) |

Also upstream: `ConsumeWindowIntoColumn {}`, `ExpelWindowFromColumn {}`, `SwapWindowLeft {}`,
`SwapWindowRight {}`, `FocusWindowUp {}`, `FocusWindowDown {}`, `FocusColumnLeft {}`,
`FocusColumnRight {}`, `FocusWindow { id: u64 }` (required, not optional).

**[V] biri fork (https://github.com/barrulus/biri, source
https://raw.githubusercontent.com/barrulus/biri/main/niri-ipc/src/lib.rs):**
- Does **not** add `MoveWindowLeft`/`MoveWindowRight` either (grep of full source: absent).
  Same `MoveWindowUp {}` / `MoveWindowDown {}` / `ConsumeOrExpelWindowLeft/Right` spellings as
  upstream.
- Adds actions: `ToggleWorkspaceVisibility { name: String, focus: bool }`,
  `HideWorkspace { name: String }`, `UnhideWorkspace { name: String, focus: bool }`,
  `ToggleWindowSticky { id: Option<u64> }`, `ToggleWindowShader { id: Option<u64> }`,
  `CycleWindowShader { id: Option<u64> }`, `ToggleOutputShader { output: Option<String> }`,
  `CycleOutputShader { output: Option<String> }`, `ToggleTouchpad {}`, `ToggleDwt {}`,
  `OverviewZoomCycle { reverse: bool }`, `OverviewZoomIn {}`, `OverviewZoomOut {}`.
- Adds requests: `WorkspacesWithHidden`, `Actions(Vec<Action>)` ("atomic sequence"),
  `CreateVirtualOutput { width: Option<u16>, height: Option<u16>, refresh_rate: Option<u32>,
  name: Option<String> }`, `RemoveVirtualOutput { name: String }`; response addition
  `VirtualOutputCreated(String)`.
- README documents the same feature set (`overview-zoom-*`, workspace hide/unhide with optional
  `focus=true`, `toggle-window-sticky`, `toggle-touchpad`/`toggle-dwt`, shader binds, virtual
  outputs via `niri msg output`); README names no renamed actions:
  https://github.com/barrulus/biri

## 4. Events — exact variants and field names

**[V]** All from niri-ipc 26.4.0 `Event` (lib.rs 1571–1694;
https://docs.rs/niri-ipc/latest/niri_ipc/enum.Event.html). Externally tagged, PascalCase variant
names, snake_case field names as written. 19 variants:

| Variant | Fields | JSON example |
|---|---|---|
| `WorkspacesChanged` | `workspaces: Vec<Workspace>` | `{"WorkspacesChanged":{"workspaces":[…]}}` |
| `WorkspaceUrgencyChanged` | `id: u64`, `urgent: bool` | |
| `WorkspaceActivated` | `id: u64`, `focused: bool` | `{"WorkspaceActivated":{"id":1,"focused":true}}` |
| `WorkspaceActiveWindowChanged` | `workspace_id: u64`, `active_window_id: Option<u64>` | |
| `WindowsChanged` | `windows: Vec<Window>` | |
| `WindowOpenedOrChanged` | `window: Window` | |
| `WindowClosed` | `id: u64` | `{"WindowClosed":{"id":4}}` |
| `WindowFocusChanged` | `id: Option<u64>` (None ⇒ no window focused) | `{"WindowFocusChanged":{"id":7}}` |
| `WindowFocusTimestampChanged` | `id: u64`, `focus_timestamp: Option<Timestamp>` | |
| `WindowUrgencyChanged` | `id: u64`, `urgent: bool` | |
| `WindowLayoutsChanged` | `changes: Vec<(u64, WindowLayout)>` — tuples ⇒ JSON arrays `[[id, layout], …]` | |
| `KeyboardLayoutsChanged` | `keyboard_layouts: KeyboardLayouts` | |
| `KeyboardLayoutSwitched` | `idx: u8` | |
| `OverviewOpenedOrClosed` | `is_open: bool` | |
| `ConfigLoaded` | `failed: bool` (always received right after connecting) | |
| `ScreenshotCaptured` | `path: Option<String>` | |
| `CastsChanged` | `casts: Vec<Cast>` | |
| `CastStartedOrChanged` | `cast: Cast` | |
| `CastStopped` | `stream_id: u64` | |

**[V] `Workspace` struct fields** (lib.rs 1408–1444 / https://docs.rs/niri-ipc/latest/niri_ipc/struct.Workspace.html) —
confirmed exactly: `id: u64`, `idx: u8`, `name: Option<String>`, `output: Option<String>`,
`is_urgent: bool`, `is_active: bool`, `is_focused: bool`, `active_window_id: Option<u64>`.
**No `is_hidden` field upstream.** The biri fork appends `is_hidden: bool` (biri lib.rs,
`Workspace` struct — "Is this workspace hidden").

**[V] `Window` struct fields** (lib.rs 1297–1337 / https://docs.rs/niri-ipc/latest/niri_ipc/struct.Window.html):
`id: u64`, `title: Option<String>`, `app_id: Option<String>`, `pid: Option<i32>`,
`workspace_id: Option<u64>`, `is_focused: bool`, `is_floating: bool`, `is_urgent: bool`,
`layout: WindowLayout`, `focus_timestamp: Option<Timestamp>`. **No `is_hidden`;** biri adds
`is_sticky: bool`. `Timestamp` = `{ secs: u64, nanos: u32 }`.

`KeyboardLayouts` = `{ names: Vec<String>, current_idx: u8 }`. `Overview` = `{ is_open: bool }`.

## 5. Outputs request & output shape

**[V]** Request: unit variant `Outputs` → wire `"Outputs"`. Reply:
`{"Ok":{"Outputs":{"DP-1":{…}}}}` — `Response::Outputs(HashMap<String, Output>)`, keyed by
output name (lib.rs 145; https://docs.rs/niri-ipc/latest/niri_ipc/enum.Response.html).

**[V] `Output` struct** (lib.rs 1204–1231 / https://docs.rs/niri-ipc/latest/niri_ipc/struct.Output.html):
`name: String`, `make: String`, `model: String`, `serial: Option<String>`,
`physical_size: Option<(u32, u32)>` (mm; JSON array `[w,h]` or null), `modes: Vec<Mode>`,
`current_mode: Option<usize>` (index into `modes`; None if disabled), `is_custom_mode: bool`,
`vrr_supported: bool`, `vrr_enabled: bool`, `logical: Option<LogicalOutput>`.

**[V] `Mode`** (lib.rs 1236–1245): `width: u16`, `height: u16`, `refresh_rate: u32`
(**millihertz**), `is_preferred: bool`.

**[V] `LogicalOutput`** (lib.rs 1250–1263 / https://docs.rs/niri-ipc/latest/niri_ipc/struct.LogicalOutput.html):
`x: i32`, `y: i32`, `width: u32`, `height: u32`, `scale: f64`, `transform: Transform`.

**[V] `Transform` serde names** (lib.rs 1269–1292 / https://docs.rs/niri-ipc/latest/niri_ipc/enum.Transform.html)
— exact wire strings, note the explicit renames only on the rotations:

| Rust variant | serde JSON name |
|---|---|
| `Normal` | `"Normal"` |
| `_90` | `"90"` (`#[serde(rename = "90")]`) |
| `_180` | `"180"` (`#[serde(rename = "180")]`) |
| `_270` | `"270"` (`#[serde(rename = "270")]`) |
| `Flipped` | `"Flipped"` |
| `Flipped90` | `"Flipped90"` (clap-only alias `flipped-90`; serde name unchanged) |
| `Flipped180` | `"Flipped180"` |
| `Flipped270` | `"Flipped270"` |

(Rotation goes counter-clockwise per the doc comment.)

**[V] Transient output config**: `Request::Output { output: String, action: OutputAction }` →
reply `{"Ok":{"OutputConfigChanged":"Applied"|"OutputWasMissing"}}`
(`OutputConfigChanged` enum, lib.rs 1398–1403). `OutputAction` variants include `Off`, `On`,
`Mode { mode: ModeToSet }`, … (lib.rs 1018+).

## 6. Framing: newline-delimited JSON; event-stream connection quirk

**[V] Newline-delimited, one JSON value per line, both directions.**
Protocol doc (https://niri-wm.github.io/niri/IPC.html): connect to the UNIX socket at
`$NIRI_SOCKET`; "Write your request encoded in JSON on a single line, followed by a newline
character, or by flushing and shutting down the write end of the connection. Read the reply as
JSON, also on a single line."
Server source (niri `src/ipc/server.rs`, `handle_client`): reads with
`read.read_until(b'\n', &mut buf)`, writes the reply via
`serde_json::to_writer(&mut buf, &reply); buf.push(b'\n'); write.write_all(&buf)`.

**[V] Event-stream connection stops accepting requests.** After the server sees
`Request::EventStream` it (a) writes the normal one-line reply `{"Ok":"Handled"}`, (b) sends the
**full current state up-front** (initial burst from `EventStreamState::replicate()`:
`WorkspacesChanged`, `WindowsChanged`, etc.), (c) spawns a task that writes each following
`Event` as one JSON line (`serde_json::to_vec(&event)` + `b'\n'`), and (d) **`return`s out of the
request-read loop** — that socket is then events-only until closed
(server.rs `handle_client`, `requested_event_stream` branch ends with `return Ok(())`; event
write loop in `handle_event_stream_client`).
Biri's doc comment states the same contract: reply `Reply::Ok(Response::Handled)`, then events
one per line, full state up-front so clients never desync
(https://raw.githubusercontent.com/barrulus/biri/main/niri-ipc/src/lib.rs).

**[V] Slow-consumer drop**: events are queued in a bounded channel of
`EVENT_STREAM_BUFFER_SIZE = 64`; if a client falls behind and the buffer fills, the server
disconnects that event-stream client (server.rs lines 41–43, 120–135: "disconnecting IPC event
stream client"). Client implementations must read promptly.

**[V] Forward compatibility**: "The JSON output is meant to stay backwards-compatible" —
unknown fields/variants may be added; `niri msg --json`, `niri msg raw-request`, and
`niri msg --print-request` are the official inspection tools
(https://niri-wm.github.io/niri/IPC.html).

---

## Answers to the six questions (compact)

1. **Request envelope [V]**: externally tagged, PascalCase. `{"Action":{"MoveWindowUp":{}}}` is
   correct. `{"EventStream":{}}` is **wrong** — unit variants serialize as bare strings:
   `"EventStream"`. (Empirically verified with serde 1.0.228.)
2. **Response envelope [V]**: `Reply = Result<Response, String>` → `{"Ok": …}` / `{"Err": "…"}`.
   Actions reply `{"Ok":"Handled"}` (unit variant ⇒ string, not `{"Handled":{}}`); data requests
   reply `{"Ok":{"<Variant>":payload}}`; EventStream replies `{"Ok":"Handled"}` once, then
   streams event lines.
3. **Action variants [V]**: upstream & biri both have `MoveWindowUp {}`, `MoveWindowDown {}`,
   `ConsumeOrExpelWindowLeft { id: Option<u64> }`, `ConsumeOrExpelWindowRight { id: Option<u64> }`.
   **No** `MoveWindowLeft`/`MoveWindowRight` in either — horizontal movement is `MoveColumn*`.
   biri adds sticky/shader/touchpad/DWT/overview-zoom/workspace-visibility actions (§3).
4. **Events [V]**: `WorkspacesChanged { workspaces: Vec<Workspace> }`,
   `WorkspaceActivated { id, focused }`, `WindowFocusChanged { id: Option<u64> }` (plus 16 more,
   §4). Workspace fields: `id, idx, name, output, is_urgent, is_active, is_focused,
   active_window_id` — **no `is_hidden` upstream** (biri adds it last). No `rename_all`
   anywhere; names are exactly as written.
5. **Outputs [V]**: request `"Outputs"`; response `{"Ok":{"Outputs":{"<name>":Output}}}`;
   `Output.logical.transform` ∈ `"Normal" | "90" | "180" | "270" | "Flipped" | "Flipped90" |
   "Flipped180" | "Flipped270"`.
6. **Framing [V]**: newline-delimited JSON both directions; event-stream connection replies
   once, dumps full state, then is events-only (server returns from its read loop) and drops
   slow clients after a 64-event backlog.
