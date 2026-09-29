//! In-memory mirror of the compositor's workspace/window state.
//!
//! Phase 1 seeds this from the event stream's full-state dump and applies the
//! deltas that follow. The router (Phase 3) reads
//! [`StateTracker::focused_output`] and friends to pick the right action for
//! whichever output currently has focus.

use std::collections::HashMap;

use crate::protocol::{Event, Transform, Window, Workspace};

/// The main scrolling axis of an output, derived from its [`Transform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    /// Lowercase name used in `nah status` and daemon logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Axis::Horizontal => "horizontal",
            Axis::Vertical => "vertical",
        }
    }
}

/// Pure mapping from an output transform to the scrolling main axis.
///
/// A 90/270 degree rotation, flipped or not, turns the layout strip vertical;
/// `Normal` and `180` (flipped or not) keep it horizontal.
pub const fn transform_to_axis(transform: Transform) -> Axis {
    match transform {
        Transform::Normal | Transform::Rotate180 | Transform::Flipped | Transform::Flipped180 => {
            Axis::Horizontal
        }
        Transform::Rotate90
        | Transform::Rotate270
        | Transform::Flipped90
        | Transform::Flipped270 => Axis::Vertical,
    }
}

/// Tracker-side view of one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    pub id: u64,
    pub idx: u8,
    pub name: Option<String>,
    /// `None` for headless workspaces.
    pub output: Option<String>,
    pub is_urgent: bool,
    pub is_active: bool,
    pub is_focused: bool,
    pub active_window_id: Option<u64>,
    pub is_hidden: bool,
}

impl From<&Workspace> for WorkspaceState {
    fn from(workspace: &Workspace) -> Self {
        Self {
            id: workspace.id,
            idx: workspace.idx,
            name: workspace.name.clone(),
            output: workspace.output.clone(),
            is_urgent: workspace.is_urgent,
            is_active: workspace.is_active,
            is_focused: workspace.is_focused,
            active_window_id: workspace.active_window_id,
            is_hidden: workspace.is_hidden,
        }
    }
}

/// Tracker-side view of one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowState {
    pub id: u64,
    /// `None` for floating/unmapped windows.
    pub workspace_id: Option<u64>,
    pub is_focused: bool,
    pub is_floating: bool,
}

impl From<&Window> for WindowState {
    fn from(window: &Window) -> Self {
        Self {
            id: window.id,
            workspace_id: window.workspace_id,
            is_focused: window.is_focused,
            is_floating: window.is_floating,
        }
    }
}

/// Live `workspace -> output` map plus `window -> workspace` model.
#[derive(Debug, Default)]
pub struct StateTracker {
    workspaces: HashMap<u64, WorkspaceState>,
    windows: HashMap<u64, WindowState>,
    focused_window: Option<u64>,
}

impl StateTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one event. Unknown/unmodeled events are ignored.
    pub fn update(&mut self, event: &Event) {
        match event {
            Event::WorkspacesChanged { workspaces } => self.apply_workspaces(workspaces),
            Event::WorkspaceActivated { id, focused } => {
                self.apply_workspace_activated(*id, *focused);
            }
            Event::WorkspaceUrgencyChanged { id, urgent } => {
                if let Some(workspace) = self.workspaces.get_mut(id) {
                    workspace.is_urgent = *urgent;
                }
            }
            Event::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                if let Some(workspace) = self.workspaces.get_mut(workspace_id) {
                    workspace.active_window_id = *active_window_id;
                }
            }
            Event::WindowsChanged { windows } => self.apply_windows(windows),
            Event::WindowOpenedOrChanged { window } => self.apply_window(window),
            Event::WindowClosed { id } => self.apply_window_closed(*id),
            Event::WindowFocusChanged { id } => self.apply_window_focus(*id),
            // The remaining known events (timestamps, layout, keyboard, casts,
            // screenshots, overview, config) do not affect Phase 1 state.
            Event::Unknown => {}
            _ => {}
        }
    }

    /// Replace the whole workspace map (the seed event and later resyncs).
    pub fn apply_workspaces(&mut self, workspaces: &[Workspace]) {
        self.workspaces.clear();
        self.workspaces.reserve(workspaces.len());
        for workspace in workspaces {
            self.workspaces
                .insert(workspace.id, WorkspaceState::from(workspace));
        }
    }

    /// Track the focused workspace.
    ///
    /// `focused: true` moves focus to `id` (and makes it the active workspace
    /// of its output). `focused: false` clears focus for `id`; the exact
    /// runtime semantics of this arm are UNVERIFIED (not observed live), so it
    /// deliberately stays conservative.
    pub fn apply_workspace_activated(&mut self, id: u64, focused: bool) {
        if !self.workspaces.contains_key(&id) {
            return;
        }

        if !focused {
            if let Some(workspace) = self.workspaces.get_mut(&id) {
                workspace.is_focused = false;
            }
            return;
        }

        let output = self
            .workspaces
            .get(&id)
            .and_then(|workspace| workspace.output.clone());

        for (workspace_id, workspace) in self.workspaces.iter_mut() {
            workspace.is_focused = *workspace_id == id;
            if *workspace_id != id
                && workspace.is_active
                && output.is_some()
                && workspace.output == output
            {
                workspace.is_active = false;
            }
        }
        if let Some(workspace) = self.workspaces.get_mut(&id) {
            workspace.is_active = true;
        }
    }

    /// Replace the whole window map (the seed event and later resyncs).
    pub fn apply_windows(&mut self, windows: &[Window]) {
        self.windows.clear();
        self.windows.reserve(windows.len());
        self.focused_window = None;
        for window in windows {
            self.apply_window(window);
        }
    }

    /// Insert or replace one window.
    pub fn apply_window(&mut self, window: &Window) {
        if window.is_focused {
            for tracked in self.windows.values_mut() {
                tracked.is_focused = false;
            }
            self.focused_window = Some(window.id);
        } else if self.focused_window == Some(window.id) {
            self.focused_window = None;
        }
        self.windows.insert(window.id, WindowState::from(window));
    }

    /// Drop a closed window.
    pub fn apply_window_closed(&mut self, id: u64) {
        self.windows.remove(&id);
        if self.focused_window == Some(id) {
            self.focused_window = None;
        }
    }

    /// Update the focused window (`None` means no window is focused).
    pub fn apply_window_focus(&mut self, id: Option<u64>) {
        self.focused_window = id;
        for (window_id, window) in self.windows.iter_mut() {
            window.is_focused = Some(*window_id) == id;
        }
    }

    /// All tracked workspaces, in no particular order.
    #[allow(dead_code)] // Phase 3 status surface; unit-tested now.
    pub fn workspaces(&self) -> impl Iterator<Item = &WorkspaceState> {
        self.workspaces.values()
    }

    #[allow(dead_code)] // Phase 3 status surface; unit-tested now.
    pub fn workspace(&self, id: u64) -> Option<&WorkspaceState> {
        self.workspaces.get(&id)
    }

    /// The globally focused workspace, if one is known.
    pub fn focused_workspace(&self) -> Option<&WorkspaceState> {
        self.workspaces
            .values()
            .find(|workspace| workspace.is_focused)
    }

    pub fn focused_workspace_id(&self) -> Option<u64> {
        self.focused_workspace().map(|workspace| workspace.id)
    }

    /// The output of the focused workspace; this is the router's key input.
    pub fn focused_output(&self) -> Option<&str> {
        self.focused_workspace()?.output.as_deref()
    }

    /// The output a workspace lives on (`None` if headless or unknown).
    #[allow(dead_code)] // Phase 3 router surface; unit-tested now.
    pub fn output_for_workspace(&self, id: u64) -> Option<&str> {
        self.workspaces.get(&id)?.output.as_deref()
    }

    #[allow(dead_code)] // Phase 3 status surface; unit-tested now.
    pub fn window(&self, id: u64) -> Option<&WindowState> {
        self.windows.get(&id)
    }

    /// The workspace a window is assigned to; `None` for floating/unmapped
    /// windows and for unknown ids.
    #[allow(dead_code)] // Phase 3 router surface; unit-tested now.
    pub fn window_workspace(&self, id: u64) -> Option<u64> {
        self.windows.get(&id)?.workspace_id
    }

    /// The id of the focused window, if any.
    pub fn focused_window_id(&self) -> Option<u64> {
        self.focused_window
    }
}

/// State shared between the event-stream thread and the socket-server threads.
///
/// The event thread is the only writer: it applies events to the tracker and
/// seeds/refreshes the output → axis map from `Outputs`. Server threads take
/// the mutex only long enough to snapshot what a `status` reply needs or to
/// resolve one intent's axis; they never do I/O while holding it.
#[derive(Debug, Default)]
pub struct SharedState {
    tracker: StateTracker,
    output_axes: HashMap<String, Axis>,
}

impl SharedState {
    pub fn new() -> Self {
        Self {
            tracker: StateTracker::new(),
            output_axes: HashMap::new(),
        }
    }

    pub fn tracker(&self) -> &StateTracker {
        &self.tracker
    }

    pub fn tracker_mut(&mut self) -> &mut StateTracker {
        &mut self.tracker
    }

    /// The axis recorded for an output, if the output map knows it.
    pub fn axis_for_output(&self, output: &str) -> Option<Axis> {
        self.output_axes.get(output).copied()
    }

    /// The axis of the currently focused output; `None` when either the
    /// focused output or its transform is unknown (the router then falls back
    /// to the horizontal table).
    pub fn focused_axis(&self) -> Option<Axis> {
        let output = self.tracker.focused_output()?;
        self.axis_for_output(output)
    }

    /// Whether the output map already has an entry for `output`.
    pub fn contains_output(&self, output: &str) -> bool {
        self.output_axes.contains_key(output)
    }

    /// Replace the output → axis map (seeded and refreshed from `Outputs`).
    pub fn set_output_axes(&mut self, output_axes: HashMap<String, Axis>) {
        self.output_axes = output_axes;
    }

    /// Sorted `(output, axis)` snapshot for one-line refresh logs.
    pub fn output_axis_pairs(&self) -> Vec<(&str, Axis)> {
        let mut pairs: Vec<_> = self
            .output_axes
            .iter()
            .map(|(name, axis)| (name.as_str(), *axis))
            .collect();
        pairs.sort_unstable_by_key(|(name, _)| *name);
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Event;

    /// The real initial event-stream dump captured in Phase 0.
    const INITIAL_STREAM: &str = include_str!("../tests/fixtures/event-stream-initial.jsonl");

    fn seeded_tracker() -> StateTracker {
        let mut tracker = StateTracker::new();
        for line in INITIAL_STREAM.lines() {
            let event: Event = serde_json::from_str(line).expect("fixture line parses");
            tracker.update(&event);
        }
        tracker
    }

    fn floating_window(id: u64, is_focused: bool) -> Window {
        Window {
            id,
            title: Some(format!("floating {id}")),
            app_id: None,
            workspace_id: None,
            is_focused,
            is_floating: true,
        }
    }

    #[test]
    fn fixture_seeds_focused_workspace_and_output() {
        let tracker = seeded_tracker();

        // Fixture truth: the workspace with "is_focused": true is id 1, on
        // output "DP-2"; its focused window is id 6.
        assert_eq!(tracker.focused_workspace_id(), Some(1));
        assert_eq!(tracker.focused_output(), Some("DP-2"));
        assert_eq!(tracker.focused_window_id(), Some(6));
        assert_eq!(tracker.workspaces().count(), 6);
    }

    #[test]
    fn fixture_workspace_output_map_is_complete() {
        let tracker = seeded_tracker();

        let expected = [
            (1, "DP-2"),
            (2, "eDP-1"),
            (3, "HDMI-A-1"),
            (4, "DP-2"),
            (5, "HDMI-A-1"),
            (6, "eDP-1"),
        ];
        for (id, output) in expected {
            assert_eq!(
                tracker.output_for_workspace(id),
                Some(output),
                "workspace {id}"
            );
            assert!(tracker.workspace(id).is_some(), "workspace {id} missing");
        }
        assert_eq!(tracker.output_for_workspace(999), None);
        assert!(tracker.workspace(999).is_none());
    }

    #[test]
    fn fixture_window_workspace_model() {
        let tracker = seeded_tracker();

        assert_eq!(tracker.window_workspace(6), Some(1));
        assert_eq!(tracker.window_workspace(9), Some(3));
        assert_eq!(tracker.window_workspace(999), None);
    }

    #[test]
    fn floating_window_without_workspace_is_tolerated() {
        let mut tracker = seeded_tracker();

        tracker.update(&Event::WindowOpenedOrChanged {
            window: floating_window(999, false),
        });
        assert!(tracker.window(999).is_some());
        assert_eq!(tracker.window_workspace(999), None);
        assert_eq!(tracker.focused_output(), Some("DP-2"));

        // A focused floating window must not invent a workspace either.
        tracker.update(&Event::WindowOpenedOrChanged {
            window: floating_window(1000, true),
        });
        assert_eq!(tracker.focused_window_id(), Some(1000));
        assert_eq!(tracker.window_workspace(1000), None);
        assert_eq!(tracker.focused_output(), Some("DP-2"));

        tracker.update(&Event::WindowClosed { id: 1000 });
        assert_eq!(tracker.focused_window_id(), None);
        assert!(tracker.window(1000).is_none());
    }

    #[test]
    fn workspace_activated_switches_focused_output() {
        let mut tracker = seeded_tracker();

        tracker.update(&Event::WorkspaceActivated {
            id: 3,
            focused: true,
        });
        assert_eq!(tracker.focused_workspace_id(), Some(3));
        assert_eq!(tracker.focused_output(), Some("HDMI-A-1"));
        assert!(!tracker.workspace(1).expect("workspace 1").is_focused);
        assert!(tracker.workspace(3).expect("workspace 3").is_active);
        assert!(!tracker.workspace(5).expect("workspace 5").is_active);

        // `focused: false` clears focus again (conservative semantics; see
        // the doc comment on `apply_workspace_activated`).
        tracker.update(&Event::WorkspaceActivated {
            id: 3,
            focused: false,
        });
        assert_eq!(tracker.focused_output(), None);

        // Unknown workspace ids are ignored.
        tracker.update(&Event::WorkspaceActivated {
            id: 42,
            focused: true,
        });
        assert_eq!(tracker.focused_output(), None);
    }

    #[test]
    fn headless_workspace_seed_has_no_output() {
        let mut tracker = StateTracker::new();
        tracker.update(&Event::WorkspacesChanged {
            workspaces: vec![Workspace {
                id: 1,
                idx: 1,
                name: None,
                output: None,
                is_urgent: false,
                is_active: true,
                is_focused: true,
                active_window_id: None,
                is_hidden: false,
            }],
        });

        assert_eq!(tracker.focused_workspace_id(), Some(1));
        assert_eq!(tracker.focused_output(), None);
        assert_eq!(tracker.output_for_workspace(1), None);
    }

    #[test]
    fn window_focus_changed_updates_focused_window() {
        let mut tracker = seeded_tracker();

        tracker.update(&Event::WindowFocusChanged { id: Some(9) });
        assert_eq!(tracker.focused_window_id(), Some(9));
        assert!(tracker.window(9).expect("window 9").is_focused);

        tracker.update(&Event::WindowFocusChanged { id: None });
        assert_eq!(tracker.focused_window_id(), None);
        assert!(!tracker.window(9).expect("window 9").is_focused);
    }

    #[test]
    fn transform_to_axis_covers_all_eight_spellings() {
        let cases = [
            (Transform::Normal, Axis::Horizontal),
            (Transform::Rotate180, Axis::Horizontal),
            (Transform::Flipped, Axis::Horizontal),
            (Transform::Flipped180, Axis::Horizontal),
            (Transform::Rotate90, Axis::Vertical),
            (Transform::Rotate270, Axis::Vertical),
            (Transform::Flipped90, Axis::Vertical),
            (Transform::Flipped270, Axis::Vertical),
        ];

        for (transform, axis) in cases {
            assert_eq!(transform_to_axis(transform), axis, "{transform:?}");
        }
    }

    #[test]
    fn shared_state_resolves_focused_axis() {
        let mut state = SharedState::new();
        state.tracker_mut().update(&Event::WorkspacesChanged {
            workspaces: vec![
                Workspace {
                    id: 1,
                    idx: 1,
                    name: None,
                    output: Some("DP-2".to_string()),
                    is_urgent: false,
                    is_active: true,
                    is_focused: true,
                    active_window_id: None,
                    is_hidden: false,
                },
                Workspace {
                    id: 2,
                    idx: 1,
                    name: None,
                    output: Some("eDP-1".to_string()),
                    is_urgent: false,
                    is_active: false,
                    is_focused: false,
                    active_window_id: None,
                    is_hidden: false,
                },
            ],
        });

        // Unknown until the `Outputs` map has been seeded.
        assert_eq!(state.focused_axis(), None);
        assert!(!state.contains_output("DP-2"));

        let axes = HashMap::from([
            ("DP-2".to_string(), Axis::Vertical),
            ("eDP-1".to_string(), Axis::Horizontal),
        ]);
        state.set_output_axes(axes);

        assert!(state.contains_output("DP-2"));
        assert_eq!(state.focused_axis(), Some(Axis::Vertical));
        assert_eq!(state.axis_for_output("eDP-1"), Some(Axis::Horizontal));
        assert_eq!(state.axis_for_output("HDMI-A-1"), None);
        assert_eq!(
            state.output_axis_pairs(),
            vec![("DP-2", Axis::Vertical), ("eDP-1", Axis::Horizontal)]
        );
    }

    #[test]
    fn unknown_event_does_not_disturb_state() {
        let mut tracker = seeded_tracker();
        tracker.update(&Event::Unknown);

        assert_eq!(tracker.focused_output(), Some("DP-2"));
        assert_eq!(tracker.focused_workspace_id(), Some(1));
    }
}
