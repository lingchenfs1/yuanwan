//! 线上格式（A 组）：外层头、块的编解码、握手 TLV。
//! 字节布局以规格 ywp.md 和 M2 计划附录 A 为准；常量在 [`crate::consts`]。

pub mod audio;
pub mod buf;
pub mod chunk;
pub mod control;
pub mod feedback;
pub mod handshake;
pub mod header;
pub mod input;
pub mod misc;
pub mod report;
pub mod video;

pub use audio::{AudioChunk, AudioChunkOwned, AudioFrames};
pub use buf::{Reader, Writer};
pub use chunk::{validate_chunks, Chunk, ChunkError, ChunkIter, ChunkSummary, EncodeChunk};
pub use control::{parse_ack, Ack, ControlFrag, ControlFragOwned};
pub use feedback::FeedbackState;
pub use handshake::{HsErr, HsInitPayload, HsRespPayload, SessionInfo};
pub use header::{Cid, Class, OuterHeader, PacketType};
pub use input::{InputChunk, InputChunkOwned, InputEvent, InputEvents, InputState, Touch};
pub use misc::{close_reason_bytes, parse_pong, CloseOwned, PathChallenge, PathResponse, Ping, Pong};
pub use report::{
    Nack, NackOwned, NackSegments, Need, PathReport, PathReports, Report, ReportOwned, ReportVideoFields, UpPathReport,
    UpPathReports, UpReport, UpReportOwned,
};
pub use video::{
    FrameStatusEntry, FrameStatusIter, FrameStatusKind, FrameStatusList, FrameStatusOwned, VideoChunk, VideoHeader,
};
