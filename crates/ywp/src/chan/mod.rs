//! 各通道的状态机（B 组，M2b）：输入、反馈、音频。

pub mod audio;
pub mod feedback;
pub mod input;

pub use audio::AudioTx;
pub use feedback::{FeedbackRx, FeedbackTx};
pub use input::{InputAction, InputReceiver, InputSender, NeutralWhy};
