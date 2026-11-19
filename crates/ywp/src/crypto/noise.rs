//! Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s 的发起方和应答方（A 组）。
//!
//! 已核对 snow 0.10.0（第 0 步）：
//! - Builder 的 psk / local_private_key / prologue / remote_public_key 返回 `Result<Self, Error>`，
//!   build_initiator / build_responder / generate_keypair 返回 `Result`；
//! - risky-raw-split 下有 `HandshakeState::dangerously_get_raw_split() -> ([u8; 32], [u8; 32])`；
//! - `get_handshake_hash()` 给 iv 派生用；
//! - 测试用的固定临时密钥 `Builder::fixed_ephemeral_key_for_testing_only(key) -> Self`（doc(hidden)，
//!   返回 Self 不是 Result）还在，金标向量能覆盖握手；
//! - 从私钥算公钥可以用 `snow::resolvers::{CryptoResolver, DefaultResolver}::resolve_dh` 拿到
//!   `snow::types::Dh`，`set(私钥)` 后取 `pubkey()`，不必另加 x25519 依赖。
//!
//! TODO(A 组)：这里的公开接口（发起方 / 应答方的包装、HS_INIT / HS_RESP 的组包和拆包）由 A 组定，
//! 只给 endpoint 用；要给别的 crate 用时找协调人加进 §4 的签名表。
