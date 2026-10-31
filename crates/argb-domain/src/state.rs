//! Machine power states the lighting reacts to.

use serde::{Deserialize, Serialize};

/// The power state of the machine as seen by the lighting manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerState {
    /// Windows is running and a user may be present.
    Active,
    /// The machine is in a Windows sleep state. Nothing runs; devices show
    /// whatever was written to them before suspension.
    Sleep,
}

impl PowerState {
    /// Every state, in declaration order.
    pub const ALL: [PowerState; 2] = [PowerState::Active, PowerState::Sleep];
}

impl std::fmt::Display for PowerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowerState::Active => f.write_str("active"),
            PowerState::Sleep => f.write_str("sleep"),
        }
    }
}
