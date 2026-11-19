//! 线上格式（A 组）：外层头、块的编解码、握手 TLV。
//! 字节布局以规格 ywp.md（v0.2）§4.4、§5 为准；常量在 [`crate::consts`]。

// lib.rs 为第 0 步骨架整体放宽了这几项；本模块已实现完，重新打开
#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

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
pub use chunk::{is_known_tlv_chunk, validate_chunks, Chunk, ChunkError, ChunkIter, ChunkSummary, EncodeChunk};
pub use control::{parse_ack, Ack, ControlFrag, ControlFragOwned};
pub use feedback::FeedbackState;
pub use handshake::{put_tlv, HsErr, HsInitPayload, HsRespPayload, SeenSet, SessionInfo, TlvIter};
pub use header::{Cid, Class, OuterHeader, PacketType};
pub use input::{InputChunk, InputChunkOwned, InputEvent, InputEvents, InputState, Touch};
pub use misc::{
    close_reason_bytes, parse_close, parse_path8, parse_ping, parse_pong, CloseOwned, PathChallenge, PathResponse,
    Ping, Pong,
};
pub use report::{
    Nack, NackOwned, NackSegments, Need, PathReport, PathReports, Report, ReportOwned, ReportVideoFields, UpPathReport,
    UpPathReports, UpReport, UpReportOwned,
};
pub use video::{
    FrameStatusEntry, FrameStatusIter, FrameStatusKind, FrameStatusList, FrameStatusOwned, VideoChunk, VideoHeader,
};
