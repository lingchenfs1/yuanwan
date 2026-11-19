//! 平台适配。Windows 以后另写 platform/windows.rs。

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;
