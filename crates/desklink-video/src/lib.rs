pub const PACKAGE_NAME: &str = "desklink-video";

mod continuity;
mod packet;
mod queue;
pub use continuity::{KEYFRAME_RETRY_INTERVAL, VideoContinuity, VideoContinuityAction};
pub use packet::{
    AssembleResult, DropReason, EncodedFrame, FrameAssembler, encode_video_frame, packetize_frame,
};
pub use queue::LatestFrameQueue;
