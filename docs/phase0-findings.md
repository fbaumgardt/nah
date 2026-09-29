# Phase 0 findings — live niri protocol capture

Date: 2026-09-28 (23:10 local, America/New_York) · Host: `razer-blade`
Compositor: **niri 26.04 (067d5ef)** · CLI: **26.04 (067d5ef)** (versions match)

All captures are read-only (`niri msg` queries, `--print-request`, `event-stream`).
No actions were sent, no state was mutated.

## Environment

| Variable | Value |
|---|---|
| `NIRI_SOCKET` | `/run/user/1000/niri.wayland-1.40041.sock` (SET) |
| `XDG_RUNTIME_DIR` | `/run/user/1000` |
| `WAYLAND_DISPLAY` | `wayland-1` |

`NIRI_SOCKET` is set. Note the socket path is versioned/instance-specific
(`niri.wayland-1.40041.sock`) — the future daemon must read `$NIRI_SOCKET` at
runtime, never hardcode it. // UNVERIFIED whether the path survives a
compositor restart unchanged (it likely changes per instance).

## Step 1 — version

```
$ niri msg version
Compositor version: 26.04 (067d5ef)
CLI version:        26.04 (067d5ef)
EXIT=0
```

## Step 2 — action inventory and the six requested names

- `niri msg action` (no args): **EXIT=2**, stdout empty, inventory printed to
  **stderr** (12012 bytes).
- `niri msg action --help`: **EXIT=0**, identical 12012 bytes on **stdout**.
- `diff` of the two outputs: byte-identical. Saved as
  `tests/fixtures/actions-inventory.txt` (318 lines, sha256
  `27cb43cf…73b871`).

Exact wire names were verified with `niri msg --print-request action <name>`
(prints the JSON request without sending it).

| Requested variant | CLI name | Exists? | `--print-request` result (exact) |
|---|---|---|---|
| `MoveWindowLeft` | `move-window-left` | **NO** | `error: unrecognized subcommand 'move-window-left'` (EXIT=2) |
| `MoveWindowRight` | `move-window-right` | **NO** | `error: unrecognized subcommand 'move-window-right'` (EXIT=2) |
| `MoveWindowUp` | `move-window-up` | **YES** | `{"Action":{"MoveWindowUp":{}}}` (EXIT=0) |
| `MoveWindowDown` | `move-window-down` | **YES** | `{"Action":{"MoveWindowDown":{}}}` (EXIT=0) |
| `ConsumeOrExpelWindowLeft` | `consume-or-expel-window-left` | **YES** | `{"Action":{"ConsumeOrExpelWindowLeft":{"id":null}}}` (EXIT=0) |
| `ConsumeOrExpelWindowRight` | `consume-or-expel-window-right` | **YES** | `{"Action":{"ConsumeOrExpelWindowRight":{"id":null}}}` (EXIT=0) |

Existence list verbatim: MoveWindowLeft=MISSING, MoveWindowRight=MISSING,
MoveWindowUp=EXISTS, MoveWindowDown=EXISTS,
ConsumeOrExpelWindowLeft=EXISTS, ConsumeOrExpelWindowRight=EXISTS.

Closest alternatives present in this niri version (descriptions quoted from the
inventory):

| CLI name | Wire name | Description |
|---|---|---|
| `move-column-left` | `MoveColumnLeft` | "Move the focused column to the left" |
| `move-column-right` | `MoveColumnRight` | "Move the focused column to the right" |
| `swap-window-left` | `SwapWindowLeft` | "Swap focused window with one to the left" |
| `swap-window-right` | `SwapWindowRight` | "Swap focused window with one to the right" |
| `move-window-to-monitor-left` | `MoveWindowToMonitorLeft` | moves window to monitor on the left |
| `move-window-to-monitor-right` | `MoveWindowToMonitorRight` | moves window to monitor on the right |

Notes:
- Horizontal **window** movement inside a column does not exist as an action;
  only vertical (`MoveWindowUp/Down`). Horizontal equivalents operate on whole
  columns (`MoveColumnLeft/Right`) or swap windows (`SwapWindowLeft/Right`).
- `ConsumeOrExpelWindowLeft/Right` take an optional `id` field
  (`{"id":null}` = focused window).
- Use of `move-window-left`/`move-window-right` by this project must be avoided
  or emulated (e.g. `MoveColumnLeft/Right`).
- // UNVERIFIED whether the compositor's IPC Action enum would accept a
  hand-crafted `MoveWindowLeft` raw request (only the CLI parser was probed;
  probing the wire would require sending a request, which is mutating and was
  out of scope). Since the CLI is generated from the enum, absence is expected.

### CLI parsing gotcha

The `--json` flag is **global** and must precede the subcommand:
`niri msg outputs --json` fails (EXIT=2, `error: unexpected argument '--json' found`),
while `niri msg -j outputs` works (EXIT=0).

## Step 3 — outputs and transforms

`niri msg -j outputs` → `tests/fixtures/outputs.json` (EXIT=0, 6344 bytes).
Human-readable `niri msg outputs` saved as `tests/fixtures/outputs.txt` for
cross-check (EXIT=0).

| Output | JSON `logical.transform` | Text form | Logical size | Scale | Position |
|---|---|---|---|---|---|
| `eDP-1` (Thermotrex TL140BDXP02-0) | `Normal` | `normal` | 1706x960 | 1.5 | (0, 1600) |
| `DP-2` (Dell U2715H) | `90` | `90° counter-clockwise` | 1440x2560 | 1.0 | (1707, 0) |
| `HDMI-A-1` (Dell U2715H) | `Normal` | `normal` | 2560x1440 | 1.0 | (3147, 1120) |

Surprising/important:
- JSON spelling of transform values is `Normal`, `90` (quoted exactly); the
  text form is lowercase `normal` and `90° counter-clockwise`. The project must
  parse the JSON enum spellings, not the human text.
- Three outputs, one rotated 90° (DP-2), so axis/physical-direction mapping
  cannot be validated on a Normal-only setup — the DP-2 fixture is useful.
- // UNVERIFIED: JSON transform spellings for other values (90/180/270/
  Flipped/…) were not observed because no output currently uses them. The
  serde enum likely also has `"180"`, `"270"`, `"Flipped"`, `"Flipped-90"`,
  etc., but this was NOT observed on this machine.

## Step 4 — event stream

Command used (12s timeout; EXIT=124 is the expected kill for a run-forever
command):

```
$ timeout 12 niri msg --json event-stream \
    > tests/fixtures/event-stream-initial.jsonl \
    2> tests/fixtures/event-stream-stderr.txt
EXIT=124
```

The `--json` global flag before the subcommand worked; stderr is empty
(0 bytes). The initial dump is `tests/fixtures/event-stream-initial.jsonl`
(4879 bytes, **6 lines**). No further events arrived during the 12s window
(the compositor was idle; no workspace switches were triggered).

First (all) lines and variant names, in order:

| # | Variant | Top-level payload field(s) |
|---|---|---|
| 1 | `WorkspacesChanged` | `workspaces` (list of 6) |
| 2 | `WindowsChanged` | `windows` (list of 9) |
| 3 | `KeyboardLayoutsChanged` | `keyboard_layouts` |
| 4 | `OverviewOpenedOrClosed` | `is_open` = `false` |
| 5 | `ConfigLoaded` | `failed` = `false` |
| 6 | `CastsChanged` | `casts` = `[]` |

`WorkspacesChanged`: **PRESENT** — exact field name is `workspaces`.
Workspace object field names (exact):
`id`, `idx`, `name`, `output`, `is_urgent`, `is_active`, `is_focused`,
`active_window_id`, `is_hidden`.
Sample first entry:
```json
{"id":3,"idx":1,"name":null,"output":"HDMI-A-1","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":9,"is_hidden":false}
```
Note: `is_active` is per-output (HDMI-A-1 and eDP-1 and DP-2 each have exactly
one active workspace); `is_focused` marks the globally focused one. The
`is_hidden` field is present in 26.04.

`WorkspaceActivated`: **NOT PRESENT** in the initial dump (0 occurrences in the
12s capture). Because `WorkspacesChanged` carries `is_active`/`is_focused`,
initial state can be reconstructed without it. // UNVERIFIED whether
`WorkspaceActivated` fires on an actual workspace switch — not tested, since
switching workspaces mutates state and was out of scope.

`WindowsChanged` window object fields (exact): `id`, `title`, `app_id`, `pid`,
`workspace_id`, `is_focused`, `is_floating`, `is_urgent`, `is_sticky`,
`layout`, `focus_timestamp`.

Other payload samples:
```json
KeyboardLayoutsChanged => {"keyboard_layouts":{"names":["English (US)"],"current_idx":0}}
OverviewOpenedOrClosed => {"is_open":false}
ConfigLoaded => {"failed":false}
CastsChanged => {"casts":[]}
```

No `WorkspaceActivatedAtStart`-style event, no `LayerShell*`, no
`PipesChanged` appeared in the window. // UNVERIFIED what the stream emits on
changes (focus/move/workspace events) because nothing changed during capture.

## Fixture files written

| Path | Bytes | sha256 (short) |
|---|---|---|
| `tests/fixtures/actions-inventory.txt` | 12012 | `27cb43cf…73b871` |
| `tests/fixtures/outputs.json` | 6344 | `700dd4d5…61d356` |
| `tests/fixtures/outputs.txt` | 2554 | `70ce6a72…f70cb` |
| `tests/fixtures/event-stream-initial.jsonl` | 4879 | `ed730e35…5aae96` |
| `tests/fixtures/event-stream-stderr.txt` | 0 | `e3b0c442…b78552` (empty) |

## Summary of surprises

1. `MoveWindowLeft`/`MoveWindowRight` do not exist; horizontal movement is
   column-level (`MoveColumnLeft/Right`) or swap-level (`SwapWindowLeft/Right`).
2. Bare `niri msg action` prints the inventory on stderr with EXIT=2;
   `niri msg action --help` prints the identical text on stdout with EXIT=0.
3. `--json` is a global flag and must come before the subcommand.
4. `WorkspaceActivated` is absent from the initial dump; active workspace must
   be read from `WorkspacesChanged[].is_active`/`is_focused`.
5. JSON transform spelling (`Normal`, `90`) differs from the text form
   (`normal`, `90° counter-clockwise`).
6. A rotated (90°) output is present (DP-2, 1440x2560 logical), which is useful
   for axis math; no other transforms were present to observe.
