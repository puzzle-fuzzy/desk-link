pub const PACKAGE_NAME: &str = "desklink-session";

mod input;
mod state;

pub use input::{
    DesktopRect, InputSequencer, NormalizedPoint, PressedInputState, map_to_desktop,
    next_input_sequence,
};
pub use state::{SessionAction, SessionError, SessionEvent, SessionMachine, SessionState};
