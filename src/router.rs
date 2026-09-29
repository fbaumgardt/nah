//! Intent parsing and the axis-dependent routing tables.
//!
//! Intents are *screen-relative* verbs (`move-left` means "move one screen
//! slot left"), so the same intent must resolve to a different compositor
//! [`Action`] depending on the focused output's main axis.
//!
//! Both tables below were **empirically verified in the user's live niri/biri
//! session (2026-09-29)**: on rotated (vertical-main-axis) outputs the column
//! strip runs up/down the screen, so screen-left/right map to the column-level
//! actions and screen-up/down to consume-or-expel. They are deliberately
//! hardcoded; config-driven overrides are a later phase.

use std::fmt;

use crate::protocol::Action;
use crate::state::Axis;

/// A screen-relative movement request from a keybinding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Left,
    Right,
    Up,
    Down,
}

impl Intent {
    /// Every intent in routing-table order; `nah status` walks this.
    pub const ALL: [Intent; 4] = [Intent::Left, Intent::Right, Intent::Up, Intent::Down];

    /// Lowercase wire/CLI spelling of the intent.
    pub const fn as_str(self) -> &'static str {
        match self {
            Intent::Left => "move-left",
            Intent::Right => "move-right",
            Intent::Up => "move-up",
            Intent::Down => "move-down",
        }
    }

    /// Parse one intent line. Anything else is an error; the caller replies
    /// `err …` and keeps serving.
    pub fn parse(input: &str) -> Result<Self, UnknownIntent> {
        match input {
            "move-left" => Ok(Intent::Left),
            "move-right" => Ok(Intent::Right),
            "move-up" => Ok(Intent::Up),
            "move-down" => Ok(Intent::Down),
            _ => Err(UnknownIntent),
        }
    }

    /// Index into the routing tables, in `Intent::ALL` order.
    const fn index(self) -> usize {
        match self {
            Intent::Left => 0,
            Intent::Right => 1,
            Intent::Up => 2,
            Intent::Down => 3,
        }
    }
}

impl fmt::Display for Intent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The line did not name a known intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownIntent;

impl fmt::Display for UnknownIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown intent")
    }
}

impl std::error::Error for UnknownIntent {}

/// `axis = horizontal`: the strip runs left/right, so screen-left/right
/// consume-or-expel windows and screen-up/down move within a column.
///
/// Locked table, verified live (see module docs).
pub const HORIZONTAL_ROUTES: [Action; 4] = [
    Action::ConsumeOrExpelWindowLeft { id: None },
    Action::ConsumeOrExpelWindowRight { id: None },
    Action::MoveWindowUp {},
    Action::MoveWindowDown {},
];

/// `axis = vertical`: the strip runs up/down the screen, so screen-left/right
/// move whole columns and screen-up/down consume-or-expel windows.
///
/// Locked table, verified live (see module docs).
pub const VERTICAL_ROUTES: [Action; 4] = [
    Action::MoveColumnLeft {},
    Action::MoveColumnRight {},
    Action::ConsumeOrExpelWindowLeft { id: None },
    Action::ConsumeOrExpelWindowRight { id: None },
];

/// Resolve an intent against the focused output's axis.
///
/// `None` (unknown or headless focused output) falls back to the horizontal
/// table so a keypress is never dropped.
pub fn route(intent: &Intent, axis: Option<Axis>) -> Action {
    let table = match axis.unwrap_or(Axis::Horizontal) {
        Axis::Horizontal => &HORIZONTAL_ROUTES,
        Axis::Vertical => &VERTICAL_ROUTES,
    };
    table[intent.index()].clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Action, Request};

    #[test]
    fn all_eight_intent_axis_combinations() {
        let cases = [
            (
                Intent::Left,
                Axis::Horizontal,
                Action::ConsumeOrExpelWindowLeft { id: None },
            ),
            (
                Intent::Right,
                Axis::Horizontal,
                Action::ConsumeOrExpelWindowRight { id: None },
            ),
            (Intent::Up, Axis::Horizontal, Action::MoveWindowUp {}),
            (Intent::Down, Axis::Horizontal, Action::MoveWindowDown {}),
            (Intent::Left, Axis::Vertical, Action::MoveColumnLeft {}),
            (Intent::Right, Axis::Vertical, Action::MoveColumnRight {}),
            (
                Intent::Up,
                Axis::Vertical,
                Action::ConsumeOrExpelWindowLeft { id: None },
            ),
            (
                Intent::Down,
                Axis::Vertical,
                Action::ConsumeOrExpelWindowRight { id: None },
            ),
        ];

        for (intent, axis, expected) in cases {
            assert_eq!(route(&intent, Some(axis)), expected, "{intent} {axis:?}");
        }
    }

    #[test]
    fn unknown_axis_falls_back_to_horizontal() {
        for intent in Intent::ALL {
            assert_eq!(route(&intent, None), route(&intent, Some(Axis::Horizontal)));
        }
    }

    #[test]
    fn parsing_round_trips_the_supported_intents() {
        for intent in Intent::ALL {
            assert_eq!(Intent::parse(intent.as_str()), Ok(intent));
            assert_eq!(intent.to_string(), intent.as_str());
        }
    }

    #[test]
    fn unknown_intent_is_an_error() {
        for input in ["", "move", "move-left ", "MoveLeft", "status", "move-leftx"] {
            assert_eq!(Intent::parse(input), Err(UnknownIntent), "{input:?}");
        }
    }

    #[test]
    fn move_column_bytes_match_the_live_compositor() {
        // `niri msg --print-request action move-column-left` and
        // `move-column-right` were run read-only in the user's live session on
        // 2026-09-29 and printed exactly these lines (fieldless struct
        // variants serialize as `{"Name":{}}`).
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::MoveColumnLeft {})).unwrap(),
            r#"{"Action":{"MoveColumnLeft":{}}}"#
        );
        assert_eq!(
            serde_json::to_string(&Request::Action(Action::MoveColumnRight {})).unwrap(),
            r#"{"Action":{"MoveColumnRight":{}}}"#
        );
    }
}
