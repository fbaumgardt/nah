//! Wire types for the niri/biri IPC protocol.
//!
//! Every shape here is pinned against `docs/protocol-encodings.md` (verified
//! against niri-ipc 26.4.0). All enums use serde's default externally-tagged
//! representation: unit variants serialize as bare strings (`"EventStream"`)
//! and struct variants as `{"Variant":{...}}`; there is no `rename_all`.
//!
//! Structs intentionally avoid `deny_unknown_fields`, so biri's extra fields
//! (for example `Workspace::is_hidden`) and any future additions parse fine.

use std::collections::HashMap;
use std::fmt;

use serde::de::{self, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

/// A request sent to the compositor over `$NIRI_SOCKET`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Subscribe to the event stream (`"EventStream"`).
    ///
    /// The compositor replies `{"Ok":"Handled"}`, then sends the full state
    /// dump followed by deltas forever. That connection accepts no further
    /// requests, which is why the action sender uses a second connection.
    EventStream,

    /// Request all outputs (`"Outputs"`).
    Outputs,

    /// Perform a compositor action.
    Action(Action),
}

/// Compositor actions relevant to nah.
///
/// Only the variants needed so far are modeled; later phases extend this
/// against the pinned action inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    MoveWindowUp {},
    MoveWindowDown {},
    ConsumeOrExpelWindowLeft {
        id: Option<u64>,
    },
    ConsumeOrExpelWindowRight {
        id: Option<u64>,
    },
    MoveColumnLeft {},
    MoveColumnRight {},
    /// Move the window between floating and tiling; `None` = focused window.
    ///
    /// Verified against niri-ipc 26.4.0 (`ToggleWindowFloating { id:
    /// Option<u64> }`, lib.rs 811-816) and the live compositor:
    /// `niri msg --print-request action toggle-window-floating` emits
    /// `{"Action":{"ToggleWindowFloating":{"id":null}}}`. This is *not* a
    /// fieldless variant, so `id` is always present on the wire.
    ToggleWindowFloating {
        id: Option<u64>,
    },
}

impl Action {
    /// The variant name exactly as it appears on the wire; `nah status` uses
    /// it to print the resolved action for each intent.
    pub fn name(&self) -> &'static str {
        match self {
            Action::MoveWindowUp {} => "MoveWindowUp",
            Action::MoveWindowDown {} => "MoveWindowDown",
            Action::ConsumeOrExpelWindowLeft { .. } => "ConsumeOrExpelWindowLeft",
            Action::ConsumeOrExpelWindowRight { .. } => "ConsumeOrExpelWindowRight",
            Action::MoveColumnLeft {} => "MoveColumnLeft",
            Action::MoveColumnRight {} => "MoveColumnRight",
            Action::ToggleWindowFloating { .. } => "ToggleWindowFloating",
        }
    }
}

/// A compositor reply: `Ok` with a payload, or `Err` with a message.
pub type Reply = Result<Response, String>;

/// Payload of a successful reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Handled,
    Version(String),
    Outputs(HashMap<String, Output>),
    Workspaces(Vec<Workspace>),
    Windows(Vec<Window>),
    Layers(Vec<LayerSurface>),
    KeyboardLayouts(KeyboardLayouts),
    FocusedOutput(Option<Output>),
    FocusedWindow(Option<Window>),
    PickedWindow(Option<Window>),
    PickedColor(Option<PickedColor>),
    OverviewState(Overview),
    Casts(Vec<Cast>),
}

/// An output as returned by the `"Outputs"` request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub name: String,
    #[serde(default)]
    pub make: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub serial: Option<String>,
    /// Physical size in millimetres.
    #[serde(default)]
    pub physical_size: Option<(u32, u32)>,
    #[serde(default)]
    pub modes: Vec<Mode>,
    /// Index into [`Output::modes`]; `None` when the output is disabled.
    #[serde(default)]
    pub current_mode: Option<usize>,
    #[serde(default)]
    pub is_custom_mode: bool,
    #[serde(default)]
    pub vrr_supported: bool,
    #[serde(default)]
    pub vrr_enabled: bool,
    #[serde(default)]
    pub logical: Option<LogicalOutput>,
}

/// One output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mode {
    pub width: u16,
    pub height: u16,
    /// Refresh rate in millihertz.
    pub refresh_rate: u32,
    pub is_preferred: bool,
}

/// The logical (post-transform) geometry of an enabled output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicalOutput {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub transform: Transform,
}

/// Output transform; the rotated variants use numeric wire spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transform {
    Normal,
    #[serde(rename = "90")]
    Rotate90,
    #[serde(rename = "180")]
    Rotate180,
    #[serde(rename = "270")]
    Rotate270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

/// A workspace as reported by `WorkspacesChanged` and `"Workspaces"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: u64,
    #[serde(default)]
    pub idx: u8,
    #[serde(default)]
    pub name: Option<String>,
    /// Headless workspaces have no output.
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub is_urgent: bool,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub active_window_id: Option<u64>,
    /// biri extension; absent upstream, defaults to `false`.
    #[serde(default)]
    pub is_hidden: bool,
}

/// A window as reported by `WindowsChanged` / `WindowOpenedOrChanged`.
///
/// Only the fields nah tracks are modeled. `layout` and `focus_timestamp` are
/// deliberately omitted: unknown fields are ignored, which keeps parsing
/// robust if their shape changes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Window {
    pub id: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    /// `None` for floating/unmapped windows.
    #[serde(default)]
    pub workspace_id: Option<u64>,
    #[serde(default)]
    pub is_focused: bool,
    #[serde(default)]
    pub is_floating: bool,
}

/// A layout entry of `WindowLayoutsChanged`; nah does not consume it yet.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WindowLayout {}

/// Focus timestamp attached to windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timestamp {
    pub secs: u64,
    pub nanos: u32,
}

/// Result of the `"KeyboardLayouts"` request and the matching event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardLayouts {
    pub names: Vec<String>,
    pub current_idx: u8,
}

/// Overview state; nah does not consume it yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Overview {
    pub is_open: bool,
}

/// Opaque placeholder for payloads nah does not model yet. Unknown fields are
/// ignored, so any object shape parses.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cast {}

/// Opaque placeholder for layer surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LayerSurface {}

/// Opaque placeholder for picked colors.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PickedColor {}

/// An event from the compositor's event stream.
///
/// The variants match niri-ipc 26.4.0 exactly. Unknown variants deserialize to
/// [`Event::Unknown`] instead of failing, so a newer compositor cannot break
/// the stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    WorkspacesChanged {
        workspaces: Vec<Workspace>,
    },
    WorkspaceUrgencyChanged {
        id: u64,
        urgent: bool,
    },
    WorkspaceActivated {
        id: u64,
        focused: bool,
    },
    WorkspaceActiveWindowChanged {
        workspace_id: u64,
        active_window_id: Option<u64>,
    },
    WindowsChanged {
        windows: Vec<Window>,
    },
    WindowOpenedOrChanged {
        window: Window,
    },
    WindowClosed {
        id: u64,
    },
    WindowFocusChanged {
        id: Option<u64>,
    },
    WindowFocusTimestampChanged {
        id: u64,
        focus_timestamp: Option<Timestamp>,
    },
    WindowUrgencyChanged {
        id: u64,
        urgent: bool,
    },
    WindowLayoutsChanged {
        changes: Vec<(u64, WindowLayout)>,
    },
    KeyboardLayoutsChanged {
        keyboard_layouts: KeyboardLayouts,
    },
    KeyboardLayoutSwitched {
        idx: u8,
    },
    OverviewOpenedOrClosed {
        is_open: bool,
    },
    ConfigLoaded {
        failed: bool,
    },
    ScreenshotCaptured {
        path: Option<String>,
    },
    CastsChanged {
        casts: Vec<Cast>,
    },
    CastStartedOrChanged {
        cast: Cast,
    },
    CastStopped {
        stream_id: u64,
    },
    /// A variant this build does not know about; ignored by consumers.
    Unknown,
}

// Payload shapes used while parsing an `Event` one map entry at a time. They
// exist so the visitor can deserialize straight into the right fields without
// materializing an intermediate `serde_json::Value`.
#[derive(Deserialize)]
struct WorkspacesChangedPayload {
    workspaces: Vec<Workspace>,
}

#[derive(Deserialize)]
struct WorkspaceUrgencyChangedPayload {
    id: u64,
    urgent: bool,
}

#[derive(Deserialize)]
struct WorkspaceActivatedPayload {
    id: u64,
    focused: bool,
}

#[derive(Deserialize)]
struct WorkspaceActiveWindowChangedPayload {
    workspace_id: u64,
    active_window_id: Option<u64>,
}

#[derive(Deserialize)]
struct WindowsChangedPayload {
    windows: Vec<Window>,
}

#[derive(Deserialize)]
struct WindowOpenedOrChangedPayload {
    window: Window,
}

#[derive(Deserialize)]
struct WindowClosedPayload {
    id: u64,
}

#[derive(Deserialize)]
struct WindowFocusChangedPayload {
    id: Option<u64>,
}

#[derive(Deserialize)]
struct WindowFocusTimestampChangedPayload {
    id: u64,
    focus_timestamp: Option<Timestamp>,
}

#[derive(Deserialize)]
struct WindowUrgencyChangedPayload {
    id: u64,
    urgent: bool,
}

#[derive(Deserialize)]
struct WindowLayoutsChangedPayload {
    changes: Vec<(u64, WindowLayout)>,
}

#[derive(Deserialize)]
struct KeyboardLayoutsChangedPayload {
    keyboard_layouts: KeyboardLayouts,
}

#[derive(Deserialize)]
struct KeyboardLayoutSwitchedPayload {
    idx: u8,
}

#[derive(Deserialize)]
struct OverviewOpenedOrClosedPayload {
    is_open: bool,
}

#[derive(Deserialize)]
struct ConfigLoadedPayload {
    failed: bool,
}

#[derive(Deserialize)]
struct ScreenshotCapturedPayload {
    path: Option<String>,
}

#[derive(Deserialize)]
struct CastsChangedPayload {
    casts: Vec<Cast>,
}

#[derive(Deserialize)]
struct CastStartedOrChangedPayload {
    cast: Cast,
}

#[derive(Deserialize)]
struct CastStoppedPayload {
    stream_id: u64,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EventVisitor;

        impl<'de> Visitor<'de> for EventVisitor {
            type Value = Event;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a niri event object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Event, A::Error>
            where
                A: MapAccess<'de>,
            {
                let tag: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty event object"))?;

                let event = match tag.as_str() {
                    "WorkspacesChanged" => Event::WorkspacesChanged {
                        workspaces: map.next_value::<WorkspacesChangedPayload>()?.workspaces,
                    },
                    "WorkspaceUrgencyChanged" => {
                        let payload = map.next_value::<WorkspaceUrgencyChangedPayload>()?;
                        Event::WorkspaceUrgencyChanged {
                            id: payload.id,
                            urgent: payload.urgent,
                        }
                    }
                    "WorkspaceActivated" => {
                        let payload = map.next_value::<WorkspaceActivatedPayload>()?;
                        Event::WorkspaceActivated {
                            id: payload.id,
                            focused: payload.focused,
                        }
                    }
                    "WorkspaceActiveWindowChanged" => {
                        let payload = map.next_value::<WorkspaceActiveWindowChangedPayload>()?;
                        Event::WorkspaceActiveWindowChanged {
                            workspace_id: payload.workspace_id,
                            active_window_id: payload.active_window_id,
                        }
                    }
                    "WindowsChanged" => Event::WindowsChanged {
                        windows: map.next_value::<WindowsChangedPayload>()?.windows,
                    },
                    "WindowOpenedOrChanged" => Event::WindowOpenedOrChanged {
                        window: map.next_value::<WindowOpenedOrChangedPayload>()?.window,
                    },
                    "WindowClosed" => Event::WindowClosed {
                        id: map.next_value::<WindowClosedPayload>()?.id,
                    },
                    "WindowFocusChanged" => Event::WindowFocusChanged {
                        id: map.next_value::<WindowFocusChangedPayload>()?.id,
                    },
                    "WindowFocusTimestampChanged" => {
                        let payload = map.next_value::<WindowFocusTimestampChangedPayload>()?;
                        Event::WindowFocusTimestampChanged {
                            id: payload.id,
                            focus_timestamp: payload.focus_timestamp,
                        }
                    }
                    "WindowUrgencyChanged" => {
                        let payload = map.next_value::<WindowUrgencyChangedPayload>()?;
                        Event::WindowUrgencyChanged {
                            id: payload.id,
                            urgent: payload.urgent,
                        }
                    }
                    "WindowLayoutsChanged" => Event::WindowLayoutsChanged {
                        changes: map.next_value::<WindowLayoutsChangedPayload>()?.changes,
                    },
                    "KeyboardLayoutsChanged" => Event::KeyboardLayoutsChanged {
                        keyboard_layouts: map
                            .next_value::<KeyboardLayoutsChangedPayload>()?
                            .keyboard_layouts,
                    },
                    "KeyboardLayoutSwitched" => Event::KeyboardLayoutSwitched {
                        idx: map.next_value::<KeyboardLayoutSwitchedPayload>()?.idx,
                    },
                    "OverviewOpenedOrClosed" => Event::OverviewOpenedOrClosed {
                        is_open: map.next_value::<OverviewOpenedOrClosedPayload>()?.is_open,
                    },
                    "ConfigLoaded" => Event::ConfigLoaded {
                        failed: map.next_value::<ConfigLoadedPayload>()?.failed,
                    },
                    "ScreenshotCaptured" => Event::ScreenshotCaptured {
                        path: map.next_value::<ScreenshotCapturedPayload>()?.path,
                    },
                    "CastsChanged" => Event::CastsChanged {
                        casts: map.next_value::<CastsChangedPayload>()?.casts,
                    },
                    "CastStartedOrChanged" => Event::CastStartedOrChanged {
                        cast: map.next_value::<CastStartedOrChangedPayload>()?.cast,
                    },
                    "CastStopped" => Event::CastStopped {
                        stream_id: map.next_value::<CastStoppedPayload>()?.stream_id,
                    },
                    _ => {
                        map.next_value::<IgnoredAny>()?;
                        Event::Unknown
                    }
                };

                // External tagging means exactly one entry; drain any extra
                // ones instead of tripping over them.
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}

                Ok(event)
            }
        }

        deserializer.deserialize_map(EventVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_request_variants_are_bare_strings() {
        assert_eq!(
            serde_json::to_string(&Request::EventStream).unwrap(),
            "\"EventStream\""
        );
        assert_eq!(
            serde_json::to_string(&Request::Outputs).unwrap(),
            "\"Outputs\""
        );
    }

    #[test]
    fn action_encodings_match_pinned_wire_shapes() {
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::MoveWindowUp {})).unwrap(),
            r#"{"Action":{"MoveWindowUp":{}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::ConsumeOrExpelWindowLeft {
                id: None
            }))
            .unwrap(),
            r#"{"Action":{"ConsumeOrExpelWindowLeft":{"id":null}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::ConsumeOrExpelWindowLeft {
                id: Some(5)
            }))
            .unwrap(),
            r#"{"Action":{"ConsumeOrExpelWindowLeft":{"id":5}}}"#
        );
    }

    #[test]
    fn move_column_encodings_match_live_compositor() {
        // Verified read-only in the live session on 2026-09-29 with
        // `niri msg --print-request action move-column-left` (and `-right`),
        // which printed exactly these bytes.
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::MoveColumnLeft {})).unwrap(),
            r#"{"Action":{"MoveColumnLeft":{}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::MoveColumnRight {})).unwrap(),
            r#"{"Action":{"MoveColumnRight":{}}}"#
        );
    }

    #[test]
    fn action_names_match_wire_variants() {
        assert_eq!(Action::MoveColumnLeft {}.name(), "MoveColumnLeft");
        assert_eq!(
            Action::ConsumeOrExpelWindowRight { id: None }.name(),
            "ConsumeOrExpelWindowRight"
        );
    }

    #[test]
    fn toggle_window_floating_encoding_matches_live_compositor() {
        // `id` is `Option<u64>`, so `None` serializes as an explicit `null`;
        // niri 26.04 prints exactly this shape (see the variant's docs).
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::ToggleWindowFloating { id: None }))
                .unwrap(),
            r#"{"Action":{"ToggleWindowFloating":{"id":null}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::ToggleWindowFloating {
                id: Some(7)
            }))
            .unwrap(),
            r#"{"Action":{"ToggleWindowFloating":{"id":7}}}"#
        );
    }

    #[test]
    fn reply_encodings() {
        let handled: Reply = serde_json::from_str(r#"{"Ok":"Handled"}"#).unwrap();
        assert_eq!(handled, Ok(Response::Handled));

        let failed: Reply = serde_json::from_str(r#"{"Err":"boom"}"#).unwrap();
        assert_eq!(failed, Err("boom".to_string()));
    }

    #[test]
    fn transform_wire_spellings() {
        let cases = [
            ("Normal", Transform::Normal),
            ("90", Transform::Rotate90),
            ("180", Transform::Rotate180),
            ("270", Transform::Rotate270),
            ("Flipped", Transform::Flipped),
            ("Flipped90", Transform::Flipped90),
            ("Flipped180", Transform::Flipped180),
            ("Flipped270", Transform::Flipped270),
        ];

        for (wire, expected) in cases {
            let json = format!("\"{wire}\"");
            assert_eq!(serde_json::from_str::<Transform>(&json).unwrap(), expected);
            assert_eq!(serde_json::to_string(&expected).unwrap(), json);
        }
    }

    #[test]
    fn outputs_fixture_parses() {
        let outputs: HashMap<String, Output> =
            serde_json::from_str(include_str!("../tests/fixtures/outputs.json")).unwrap();

        assert_eq!(outputs.len(), 3);
        assert_eq!(
            outputs["DP-2"].logical.as_ref().unwrap().transform,
            Transform::Rotate90
        );
        assert_eq!(
            outputs["eDP-1"].logical.as_ref().unwrap().transform,
            Transform::Normal
        );
        assert_eq!(
            outputs["HDMI-A-1"].logical.as_ref().unwrap().transform,
            Transform::Normal
        );
    }

    #[test]
    fn unknown_event_variant_is_ignored() {
        let event: Event =
            serde_json::from_str(r#"{"SomeFutureEvent":{"whatever":[1,2]}}"#).unwrap();
        assert_eq!(event, Event::Unknown);

        let event: Event =
            serde_json::from_str(r#"{"OverviewOpenedOrClosed":{"is_open":true}}"#).unwrap();
        assert_eq!(event, Event::OverviewOpenedOrClosed { is_open: true });
    }

    #[test]
    fn unknown_struct_fields_are_ignored() {
        // `is_hidden` is biri-only; `future_field` does not exist yet.
        let workspace: Workspace = serde_json::from_str(
            r#"{"id":7,"idx":3,"name":null,"output":"DP-2","is_urgent":false,
                "is_active":false,"is_focused":false,"active_window_id":null,
                "is_hidden":true,"future_field":42}"#,
        )
        .unwrap();
        assert_eq!(workspace.id, 7);
        assert!(workspace.is_hidden);

        // Window fields may be missing/null without failing.
        let window: Window = serde_json::from_str(
            r#"{"id":9,"title":null,"app_id":null,"workspace_id":null,
                "is_focused":false,"is_floating":true}"#,
        )
        .unwrap();
        assert_eq!(window.workspace_id, None);
        assert!(window.is_floating);
    }
}
