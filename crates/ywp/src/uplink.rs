//! 上行合包（B 组，M2b）：整包 ≤ 400 B；INPUT 作载体；REPORT 每 20 ms 一次、4 ms 内搭车；
//! PING 每 100 ms 一次；NACK 优先搭车，等不到就单独发。
//!
//! TODO(B 组)：类型由 B 组定，只在 endpoint::client 内部用。
