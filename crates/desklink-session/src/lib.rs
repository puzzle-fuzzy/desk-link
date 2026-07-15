pub const PACKAGE_NAME: &str = "desklink-session";

mod input;
mod reconnect;
mod state;

pub use input::{
    DesktopRect, InputSequencer, NormalizedPoint, PressedInputState, map_to_desktop,
    next_input_sequence,
};
pub use reconnect::{
    DEFAULT_RECONNECT_BASE_DELAY, DEFAULT_RECONNECT_MAX_DELAY, DEFAULT_RECONNECT_RETRIES,
    ReconnectDecision, ReconnectPolicy, ReconnectPolicyError, ReconnectSchedule,
};
pub use state::{SessionAction, SessionError, SessionEvent, SessionMachine, SessionState};
