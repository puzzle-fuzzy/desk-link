pub const PACKAGE_NAME: &str = "desklink-video";

mod packet;
mod queue;
pub use packet::{AssembleResult, DropReason, EncodedFrame, FrameAssembler};
pub use queue::LatestFrameQueue;
