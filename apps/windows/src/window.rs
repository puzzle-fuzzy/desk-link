#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalState {
    Waiting,
    Accepted,
    Rejected,
}

pub struct HostApprovalWindow {
    state: ApprovalState,
}

impl HostApprovalWindow {
    pub fn new() -> Self {
        Self {
            state: ApprovalState::Waiting,
        }
    }

    pub fn accept(&mut self) {
        self.state = ApprovalState::Accepted;
    }

    pub fn reject(&mut self) {
        self.state = ApprovalState::Rejected;
    }

    pub fn state(&self) -> ApprovalState {
        self.state
    }
}

impl Default for HostApprovalWindow {
    fn default() -> Self {
        Self::new()
    }
}
