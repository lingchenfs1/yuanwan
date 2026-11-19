//! 加密和密钥文件的离线测试（A 组）：BLAKE2s-128、mac1、HKDF 派生 iv、AEAD 篡改、
//! 重放窗口只在 AEAD 通过后提交、keys/ 的权限和格式、配对串。
//!
//! BLAKE2s、HKDF 的期望值由 Python 标准库（hashlib.blake2s、hmac）独立算出，不是用本实现自己生成的。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use ywp::consts::*;
use ywp::creds::*;
use ywp::crypto::mac::{blake2s256, keyed_blake2s128};
use ywp::crypto::*;
use ywp::packet::{open_data, PacketWriter, PathRx};
use ywp::wire::{Cid, Class, OuterHeader, PacketType, Ping};
use ywp::DropReason;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn seq32(start: u8) -> [u8; 32] {
    let mut b = [0u8; 32];
    for (i, x) in b.iter_mut().enumerate() {
        *x = start + i as u8;
    }
    b
}

// ---------------------------------------------------------------- BLAKE2s

#[test]
fn blake2s128_uses_parameter_block() {
    let key = seq32(0);
    let data = b"YWP v0.2 blake2s-128 test";
    let got = keyed_blake2s128(&key, data);
    assert_eq!(got.to_vec(), hex("a01008c5b7fb509a010b326ea9b69aaa"));
    // 把 256 位结果截断是另一个值
    let truncated = hex("9b62c6552718c59877bd1924318ac4cf");
    assert_ne!(got.to_vec(), truncated);
}

#[test]
fn mac1_and_derived_ids() {
    let pub_ = seq32(0x40);
    let k = mac1_key(&pub_);
    assert_eq!(k.to_vec(), hex("81dd06eaa8afd940140a94adcb9fc15b2c030af1cc66c5f962b074d6583bf16f"));
    let msg: Vec<u8> = (0..1200u32).map(|i| (i * 7) as u8).collect();
    let m = mac1(&k, &msg);
    assert_eq!(m.to_vec(), hex("babec81244fcfec4adc22d4771c82609"));
    assert!(verify_mac1(&k, &msg, &m));
    let mut bad = m;
    bad[15] ^= 1;
    assert!(!verify_mac1(&k, &msg, &bad));
    assert!(!verify_mac1(&k, &msg, &m[..15]));

    let token = reset_token(&[0xA5; 32], Cid(0x0123_4567_89AB_CDEF));
    assert_eq!(token.to_vec(), hex("c7bdb86e46e3dafcf8df23c8ab38ebbb"));
    assert_eq!(gateway_id(&pub_).to_vec(), hex("df05223f22d15eb0"));
    assert_eq!(host_id("192.0.2.7").to_vec(), hex("2ba4b995ca53ae7e"));
    assert_eq!(ClientId::of(&pub_).to_string(), "df05223f");
    assert_eq!(blake2s256(&[&pub_])[..4], hex("df05223f")[..]);
}

#[test]
fn hkdf_ivs() {
    let h: Vec<u8> = (100..132).collect();
    let (c2s, s2c) = derive_ivs(&h);
    assert_eq!(c2s.to_vec(), hex("de2c447f7afa1cd623c2d641"));
    assert_eq!(s2c.to_vec(), hex("a6da0b1f5c9df56ba6089d4e"));
}

// ---------------------------------------------------------------- AEAD

fn hdr(pn: u32) -> OuterHeader {
    OuterHeader { ptype: PacketType::Data, key_phase: false, class: Class::Input, cid: Cid(7), path_id: 2, pn_trunc: pn }
}

fn ping_packet(k: &DirKey, pn: u64) -> Vec<u8> {
    let mut buf = [0u8; 64];
    let mut w = PacketWriter::new(&mut buf, 64, 0x0102_0304);
    w.push(&Ping(0xABCD)).unwrap();
    let n = w.seal(&hdr(0), pn, k);
    buf[..n].to_vec()
}

#[test]
fn aead_tamper_vectors() {
    let k = DirKey::new([0x42; 32], [0x24; 12]);
    let good = ping_packet(&k, 5);
    assert_eq!(good.len(), 39);
    assert_eq!(good[13], 5, "外层头的 PN 取 pn 的低 32 位");
    // 每一个字节翻最低位：外层头（Flags 的 Class、CID、path_id、PN 都进 AD 或 nonce）、密文、标签，全部 AeadFail
    for i in 0..good.len() {
        let mut p = good.clone();
        p[i] ^= 0x01;
        let mut rx = PathRx::new();
        let before = p.clone();
        assert_eq!(open_data(&mut p, &k, &mut rx).unwrap_err(), DropReason::AeadFail, "第 {i} 字节");
        assert_eq!(p, before, "失败时缓冲不变");
        assert!(rx.largest.is_none(), "失败时不提交");
        assert!(rx.window.check(5));
    }
    // 换 path_id、换 iv、换密钥都解不开
    for other in [DirKey::new([0x42; 32], [0x25; 12]), DirKey::new([0x43; 32], [0x24; 12])] {
        let mut p = good.clone();
        assert_eq!(open_data(&mut p, &other, &mut PathRx::new()).unwrap_err(), DropReason::AeadFail);
    }
    let mut p = good.clone();
    p[9] = 3;
    assert_eq!(open_data(&mut p, &k, &mut PathRx::new()).unwrap_err(), DropReason::AeadFail);
    // nonce = (00 00 00 ‖ path_id ‖ PN64) XOR iv：PN 高 32 位不同（线上只带低 32 位）也解不开
    let high = ping_packet(&k, (1 << 32) | 5);
    assert_eq!(high[10..14], good[10..14]);
    assert_ne!(high[14..], good[14..]);
}

#[test]
fn replay_commits_only_after_aead() {
    let k = DirKey::new([1; 32], [2; 12]);
    let mut rx = PathRx::new();
    // 伪造的 PN 1000 推不动 largest
    let mut forged = ping_packet(&k, 1000);
    let last = forged.len() - 1;
    forged[last] ^= 0xFF;
    assert_eq!(open_data(&mut forged, &k, &mut rx).unwrap_err(), DropReason::AeadFail);
    assert_eq!(rx.largest, None);
    // 真包照常
    let mut p = ping_packet(&k, 3);
    let o = open_data(&mut p, &k, &mut rx).unwrap();
    assert_eq!((o.pn, o.send_ts, o.new_largest), (3, 0x0102_0304, true));
    assert_eq!(o.chunks, &[0x20, 0, 2, 0xAB, 0xCD]);
    assert_eq!(rx.largest, Some(3));
    // 原样重放
    let mut p = ping_packet(&k, 3);
    assert_eq!(open_data(&mut p, &k, &mut rx).unwrap_err(), DropReason::Replay);
    // 乱序的旧包可以收，但不是 new_largest
    let mut p = ping_packet(&k, 1);
    assert!(!open_data(&mut p, &k, &mut rx).unwrap().new_largest);
    let mut p = ping_packet(&k, 5000);
    open_data(&mut p, &k, &mut rx).unwrap();
    let mut p = ping_packet(&k, 2);
    assert_eq!(open_data(&mut p, &k, &mut rx).unwrap_err(), DropReason::TooOld);
}

#[test]
fn open_data_header_checks() {
    let k = DirKey::new([1; 32], [2; 12]);
    let mut p = ping_packet(&k, 0);
    assert_eq!(open_data(&mut p[..33], &k, &mut PathRx::new()).unwrap_err(), DropReason::TooShort);
    let mut big = vec![0x40u8; RECV_MAX + 1];
    assert_eq!(open_data(&mut big, &k, &mut PathRx::new()).unwrap_err(), DropReason::TooLong);
    p[0] |= FLAGS_K_BIT;
    assert_eq!(open_data(&mut p, &k, &mut PathRx::new()).unwrap_err(), DropReason::KeyPhase);
    p[0] = FLAGS_HS_RESP;
    assert_eq!(open_data(&mut p, &k, &mut PathRx::new()).unwrap_err(), DropReason::BadHeader);
    p[0] = 0x00;
    assert_eq!(open_data(&mut p, &k, &mut PathRx::new()).unwrap_err(), DropReason::BadVersion);
    p[0] = 0x78;
    assert_eq!(open_data(&mut p, &k, &mut PathRx::new()).unwrap_err(), DropReason::ReservedType);
}

#[test]
fn tai64n_encoding() {
    let t = Tai64N::from_unix(1_700_000_000, 123_456_789);
    let b = t.to_bytes();
    assert_eq!(b[..8], ((1u64 << 62) + 1_700_000_000).to_be_bytes());
    assert_eq!(b[8..], 123_456_789u32.to_be_bytes());
    assert!(t.plus_micros(1) > t);
    assert!(Tai64N::next_after(Some(t)) > t);
}

// ---------------------------------------------------------------- keys/

fn tmpdir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ywp-creds-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    d
}

fn mode(p: &std::path::Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn keys_dir_lifecycle() {
    let dir = tmpdir("life");
    let gw_pub = init_keys(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join(GATEWAY_KEY_FILE)), 0o600);
    assert_eq!(mode(&dir.join(RESET_KEY_FILE)), 0o600);
    // 再 keygen 一次不覆盖
    assert!(matches!(init_keys(&dir), Err(KeyError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists));

    let keys = load_keys(&dir).unwrap();
    assert_eq!(keys.static_kp.public(), gw_pub);
    assert!(keys.clients.is_empty(), "clients 文件不存在等于没有客户端");

    let a = StaticKeypair::generate();
    let b = StaticKeypair::generate();
    let psk_a = add_client(&dir, "mac-mini", &a.public()).unwrap();
    add_client(&dir, "laptop", &b.public()).unwrap();
    assert_eq!(mode(&dir.join(CLIENTS_FILE)), 0o600);
    assert!(matches!(add_client(&dir, "mac-mini", &[9; 32]), Err(KeyError::Conflict(_))));
    assert!(matches!(add_client(&dir, "other", &a.public()), Err(KeyError::Conflict(_))));
    assert!(matches!(add_client(&dir, "有 空格", &[9; 32]), Err(KeyError::Conflict(_))));
    assert!(matches!(add_client(&dir, &"x".repeat(33), &[9; 32]), Err(KeyError::Conflict(_))));

    let keys = load_keys(&dir).unwrap();
    assert_eq!(keys.clients.len(), 2);
    assert_eq!(keys.clients[0].name, "mac-mini");
    assert_eq!(keys.clients[0].public, a.public());
    assert_eq!(keys.clients[0].psk, psk_a);

    // Debug 里看不到任何秘密
    let dbg = format!("{keys:?}");
    for secret in [to_hex(psk_a.expose()), to_hex(keys.reset_key.expose())] {
        assert!(!dbg.contains(&secret));
    }
    assert!(dbg.contains(&ClientId::of(&a.public()).to_string()), "日志里只记名字和指纹");
    let file = fs::read_to_string(dir.join(GATEWAY_KEY_FILE)).unwrap();
    assert!(!dbg.contains(file.lines().nth(1).unwrap()));

    // 按指纹删、按名字删
    let id = ClientId::of(&b.public()).to_string();
    assert_eq!(remove_client(&dir, &id).unwrap().as_deref(), Some("laptop"));
    assert_eq!(remove_client(&dir, "nobody").unwrap(), None);
    assert_eq!(remove_client(&dir, "mac-mini").unwrap().as_deref(), Some("mac-mini"));
    assert!(load_clients(&dir).unwrap().is_empty());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn keys_refuse_loose_permissions() {
    let dir = tmpdir("perm");
    init_keys(&dir).unwrap();
    add_client(&dir, "c", &[7; 32]).unwrap();

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(load_keys(&dir), Err(KeyError::BadPermissions { mode: 0o755, .. })));
    assert_eq!(mode(&dir), 0o755, "不自动改权限");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();

    for f in [GATEWAY_KEY_FILE, RESET_KEY_FILE, CLIENTS_FILE] {
        let p = dir.join(f);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(load_keys(&dir), Err(KeyError::BadPermissions { mode: 0o644, .. })), "{f}");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(load_keys(&dir), Err(KeyError::BadPermissions { .. })), "{f}");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
    }
    load_keys(&dir).unwrap();

    // 缺文件、格式不对
    fs::rename(dir.join(RESET_KEY_FILE), dir.join("moved")).unwrap();
    assert!(matches!(load_keys(&dir), Err(KeyError::Missing(_))));
    fs::rename(dir.join("moved"), dir.join(RESET_KEY_FILE)).unwrap();
    let clients = dir.join(CLIENTS_FILE);
    fs::write(&clients, "c 00 11\n").unwrap();
    assert!(matches!(load_keys(&dir), Err(KeyError::BadFormat { .. })));
    fs::write(&clients, format!("# 注释\n\na {} {}\nb {} {}\n", "1".repeat(64), "2".repeat(64), "1".repeat(64), "3".repeat(64)))
        .unwrap();
    assert!(matches!(load_keys(&dir), Err(KeyError::BadFormat { what, .. }) if what.contains("重复")));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn key_file_roundtrip() {
    let dir = tmpdir("kf");
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join("client.key");
    let s = Secret32::new([0x5A; 32]);
    write_key_file(&p, &s, "测试").unwrap();
    assert_eq!(mode(&p), 0o600);
    assert_eq!(read_key_file(&p).unwrap(), s);
    assert!(write_key_file(&p, &s, "测试").is_err(), "不覆盖已有文件");
    fs::remove_dir_all(&dir).unwrap();
}

// ---------------------------------------------------------------- 配对串

#[test]
fn pairing_string() {
    let gw = [0xA1u8; 32];
    let psk = Secret32::new([0xB2; 32]);
    for hint in [None, Some("192.0.2.10:9420"), Some("网关.example:9420")] {
        let s = encode_pairing(&gw, &psk, hint);
        assert!(s.starts_with(PAIRING_PREFIX));
        let info = decode_pairing(&format!("  {s}\n")).unwrap();
        assert_eq!(info.gw_pub, gw);
        assert_eq!(info.psk, psk);
        assert_eq!(info.entry_hint.as_deref(), hint);
        assert!(!format!("{info:?}").contains(&to_hex(psk.expose())));
    }
    let s = encode_pairing(&gw, &psk, Some("192.0.2.10:9420"));
    // 改一个字符：校验不符或解不开
    let mut chars: Vec<char> = s.chars().collect();
    let i = PAIRING_PREFIX.len() + 10;
    chars[i] = if chars[i] == 'A' { 'B' } else { 'A' };
    let bad: String = chars.into_iter().collect();
    assert!(matches!(decode_pairing(&bad), Err(KeyError::BadPairing(_))));
    // 漏字符
    assert!(matches!(decode_pairing(&s[..s.len() - 3]), Err(KeyError::BadPairing(_))));
    assert!(matches!(decode_pairing("ywp2-AAAA"), Err(KeyError::BadPairing(_))));
    assert!(matches!(decode_pairing(""), Err(KeyError::BadPairing(_))));
    // 超长的入口提示截到 255 B
    let long = "a".repeat(300);
    let info = decode_pairing(&encode_pairing(&gw, &psk, Some(&long))).unwrap();
    assert_eq!(info.entry_hint.unwrap().len(), 255);
}
