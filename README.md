# nah

Output-aware keybind router for niri/biri. One set of window-move keybinds that
keeps its screen-relative meaning on every monitor, including rotated ones. No
config file: the routing tables are fixed in `src/router.rs` and everything
else follows from the compositor's live state.

## Why

niri's movement actions are *axis-relative*: `consume-or-expel-window-left`
and `move-window-up` operate along the layout strip, not the screen. On a
rotated (portrait) output — or under biri's `main-axis "vertical"` — the strip
runs up/down the screen, so the same keybinds that consume/expel left-right on
a normal monitor move windows up-down on it. A window therefore travels in a
different screen direction depending on which monitor has focus. `nah` watches
the focused output's orientation and translates one fixed set of
screen-relative intents (`move-left/right/up/down`) into the correct action
for that output, so the same chord always means "move one screen slot in this
direction".

## How it works

`nah --daemon` is a long-lived background process; `nah <intent>` is a thin
per-keypress client (std-only, no dependencies) that connects over a private
socket. The daemon holds **three** connections to the compositor:

1. a persistent **event-stream** connection feeding the in-memory state model
   (workspaces, windows, focused output); the compositor stops reading
   requests on this connection after the `"EventStream"` handshake, which is
   why it is dedicated;
2. a persistent **action-sender** connection used exclusively to send
   `{"Action":{…}}` requests;
3. short-lived, on-demand **Outputs** connections that refresh the
   output → main-axis map from each output's `logical.transform`.

Socket protocol details and serde encodings are pinned in
`docs/protocol-encodings.md`; live captures from the machine this was built
on are in `docs/phase0-findings.md`.

The daemon answers requests on `$XDG_RUNTIME_DIR/nah.sock` (created with mode
`0600`; peers are verified via `SO_PEERCRED` and closed if their uid differs).
A stale socket is replaced; if a live daemon is already accepting
connections, a second instance exits non-zero instead of stealing the socket
(see *systemd fallback* below).

### Routing tables (locked)

Intents are screen-relative. Each resolves to one compositor `Action`
depending on the focused output's main axis, derived from its transform:
`90`, `270`, flipped 90, flipped 270 → vertical; normal, `180`, flipped
normal, flipped 180 → horizontal.

| Intent       | Horizontal output            | Vertical output              |
| ------------ | ---------------------------- | ---------------------------- |
| `move-left`  | `ConsumeOrExpelWindowLeft`   | `MoveColumnLeft`             |
| `move-right` | `ConsumeOrExpelWindowRight`  | `MoveColumnRight`            |
| `move-up`    | `MoveWindowUp`               | `ConsumeOrExpelWindowLeft`   |
| `move-down`  | `MoveWindowDown`             | `ConsumeOrExpelWindowRight`  |

Every action above was confirmed to exist in the running niri 26.04 session
(`niri msg --print-request`, see `docs/phase0-findings.md`) and in the
`niri-ipc` serde encodings (`docs/protocol-encodings.md`).
`MoveWindowLeft`/`MoveWindowRight` do **not** exist in niri 26.04 — horizontal
movement is only column-level — so the vertical table maps screen-left/right
to `MoveColumnLeft`/`MoveColumnRight`. `ConsumeOrExpelWindow*` is always sent
with `{"id":null}`, i.e. for the focused window. The tables are locked
constants in `src/router.rs` and are unit-tested byte-for-byte against the
live compositor's wire format.

If the focused output is unknown or headless — or its transform is not known
yet — the intent resolves to the **horizontal** table. A keypress is never
dropped.

## Install

Requires a stable Rust toolchain (edition 2021).

```sh
cargo build --release
install -m755 target/release/nah ~/.local/bin/nah
```

On NixOS, prefer the home-manager flake path in *Packaging (Nix)* below.

**Use absolute paths in every niri bind.** `spawn` runs no shell and does not
search your shell's `PATH`; niri 26.04 also **leaks one child process for
every failed `spawn`** — a bare `spawn "nah"` bind that cannot resolve ENOENTs
on every press and piles up children until the machine dies. This was
observed live on 2026-09-29 (see the incident note in *Limitations* and the
full write-up at the top of `docs/keybinds.kdl`). Never ship a bind that can
fail to exec. To check what the compositor can resolve:

```sh
tr '\0' '\n' < /proc/$(pgrep -x niri)/environ | grep '^PATH='
```

`docs/keybinds.kdl` is the ready-made config snippet: a `spawn-at-startup`
line plus `Mod+Shift+H/L/K/J` binds (arrow-key and bare-modifier variants
included, all with absolute paths). Paste the marked lines into your niri
config; start the daemon with the compositor:

```kdl
spawn-at-startup "/home/YOU/.local/bin/nah" "--daemon"
```

No `import-environment` line is needed on this path: niri spawns the daemon
itself, so it inherits `$NIRI_SOCKET` (and a fresh one after every compositor
restart).

## Packaging (Nix)

The repository is a flake: it exposes `packages.<system>.default` (and an
`overlays.default` for convenience), built with
`rustPlatform.buildRustPackage` from the committed `Cargo.lock`. The full
49-test suite runs inside the sandboxed build (`doCheck = true`).

```nix
# flake.nix
{
  inputs.nah.url = "github:fbaumgardt/nah/v0.1.0";
}

# home-manager
home.packages = [ inputs.nah.packages.${pkgs.system}.default ];
```

### Migration from the hand-installed copy

The manual `~/.local/bin/nah` install (see *Install*) keeps working until the
swap is done; follow this order so the compositor is never left with a daemon
whose binary you just deleted.

1. Rebuild the home-manager configuration with the `nah` input above.
2. Point `spawn-at-startup` and the four binds in the niri config at the
   profile path `/home/fbaumgardt/.nix-profile/bin/nah` (adjust the home path
   for your user). Keep it absolute: that directory *is* in niri's PATH, but
   a bind that can ever fail to exec must not be shipped (see the leak
   warning at the top of `docs/keybinds.kdl`).
3. Reboot. `spawn-at-startup` brings up the hardened daemon from the
   home-manager binary.
4. Only then remove the old copy: `rm ~/.local/bin/nah`.

## Usage

```
usage: nah --daemon
       nah --selftest
       nah status
       nah move-left | move-right | move-up | move-down
```

- `nah --daemon` — subscribe to the compositor's event stream, seed state
  from the initial full-state dump, and serve intents on
  `$XDG_RUNTIME_DIR/nah.sock`. Requires `$NIRI_SOCKET` (fail-fast with
  `NIRI_SOCKET is not set; is the compositor running?`, exit 1) and
  `$XDG_RUNTIME_DIR`.
- `nah status` — print the daemon's live state and resolved mappings:
  `focused-output`, `focused-window`, `axis` (`horizontal`/`vertical`/
  `unknown`), and the resolved `Action` for each of the four intents.
- `nah move-left|move-right|move-up|move-down` — resolve the intent through
  the focused output's axis and send the Action. The client is silent on
  success (same behavior as any native niri bind); on failure it prints one
  line to stderr (`nah: daemon not running`, `nah: timeout`, or the daemon's
  reason, e.g. `nah: busy`, `nah: niri-disconnected`) and exits 1. It waits at
  most 100 ms per reply line, never panics, and exits 2 with usage on unknown
  commands or extra arguments. One instance per keypress: fork/exec plus one
  socket round trip, a few milliseconds.
- `nah --selftest` — end-to-end check against a live compositor: verifies the
  event-stream handshake, then sends two `ToggleWindowFloating` actions over
  the dedicated sender connection (toggles the focused window's floating
  state off and back on) and reports each reply on stderr. Exit 0 only if
  both replies succeed.

## systemd fallback

Use only if you do not use `spawn-at-startup` — pick **exactly one** launch
path. If both run, the second instance hits the daemon's live-socket guard
(`$XDG_RUNTIME_DIR/nah.sock` is already accepting connections) and exits
non-zero rather than stealing the socket; with the unit's `Restart=on-failure`
it would then retry (subject to systemd's start rate limit).

```sh
install -Dm644 docs/nah.service ~/.config/systemd/user/nah.service
systemctl --user daemon-reload && systemctl --user enable --now nah.service
```

`docs/nah.service` documents the environment rules in full. The short
version: a systemd user unit does not inherit the compositor's
`$NIRI_SOCKET`, which is instance-specific per session, so the unit file's
comments walk through the `import-environment` bridge and what still needs to
be verified against your compositor before relying on it. Misconfigured,
the daemon fails fast with the one-line missing-`NIRI_SOCKET` message and
`Restart=on-failure` retries every 2 s.

## Testing

```sh
cargo test    # 49 tests: 36 unit, 8 daemon_client, 4 hardening, 1 selftest
```

No compositor needed for the suite (`cargo test --no-default-features` proves
the client path also builds std-only). `tests/common/mod.rs` implements a
mock niri IPC server speaking the real newline-delimited JSON wire protocol:
it answers `"EventStream"` with the canned full-state dump (captured live,
`tests/fixtures/`), serves `"Outputs"` from the captured fixture, records
every action line verbatim, and can drop connections on demand to exercise
reconnects. The integration tests spawn the real `nah --daemon` and `nah`
client binaries against it and assert the exact bytes on the wire, socket
permissions 0600, SIGTERM cleanup, the live-socket guard, and client exit
codes/timeout behavior.

Quality gates:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --no-default-features --all-targets -- -D warnings
```

## Limitations & notes

- **Transform changes on known outputs are not tracked.** The event stream
  does not report output changes, so the daemon re-queries `Outputs` only
  when a *new* output name appears in `WorkspacesChanged` — never otherwise,
  by design (no polling). Rotating an already-known output will not be
  noticed until a daemon restart. This is the intended trade-off against
  polling; `nah status` shows the currently believed axis.
- **Compositor gone → clean exit.** If the event connection stays down for
  30 s, the daemon logs, unlinks its socket, and exits 0. Under
  `spawn-at-startup` this is the correct end state: the restarted compositor
  spawns a fresh daemon that captures the fresh, instance-specific
  `$NIRI_SOCKET`. `NAH_EVENT_RECONNECT_TIMEOUT_MS` exists as a test-only
  override for this deadline; leave it unset in production.
- **Actions may be recorded then rejected.** The daemon treats a compositor
  `Err` reply as logged-but-delivered; only connection failures mark the
  sender down. Intents that arrive while disconnected are refused
  (`err niri-disconnected`) and dropped, never queued across reconnects.
- **The 2026-09-29 incident.** While wiring up the first binds, a bare
  `spawn "nah"` bind that was not resolvable in niri's PATH ENOENT'd on every
  press and leaked one failed-spawn child process per press under niri 26.04,
  reaching ≈12 GB of RAM plus the full 8 GB of swap before a kernel OOM
  cascade killed unrelated applications — the reason every example above and
  in `docs/keybinds.kdl` uses absolute paths.
