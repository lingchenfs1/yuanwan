//! 握手的离线测试（A 组）：在内存里跑完 Noise_IKpsk2，核对两端的密钥和 iv、HS_INIT 布局、各种失败情形、
//! 迟到的 HS_RESP、RESET 规则，以及金标向量（tests/vectors/golden.txt）。
//!
//! 要 testkit feature：cargo test -p ywp --features testkit。
//! 金标向量改了线上格式才需要重新生成：YWP_REGEN_VECTORS=1 cargo test -p ywp --features testkit --test handshake

#![cfg(feature = "testkit")]

use std::collections::HashMap;
use std::fmt::Write as _;
use ywp::consts::*;
use ywp::creds::to_hex;
use ywp::crypto::noise::HsTestHooks;
use ywp::crypto::*;
use ywp::packet::{check_reset, encode_reset, encode_reset_with, open_data, PacketWriter, PathRx};
use ywp::wire::*;
use ywp::DropReason;

struct Fixture {
    gw: StaticKeypair,
    cl: StaticKeypair,
    psk: Secret32,
    cid: Cid,
}

fn fixture() -> Fixture {
    Fixture {
        gw: StaticKeypair::from_secret([0x01; 32]),
        cl: StaticKeypair::from_secret([0x02; 32]),
        psk: Secret32::new([0x03; 32]),
        cid: Cid(0x0123_4567_89AB_CDEF),
    }
}

fn init_payload(ts: Tai64N) -> HsInitPayload {
    HsInitPayload {
        ts,
        ver: PROTO_VERSION,
        min_ver: PROTO_MIN_VERSION,
        caps: caps::SPLIT2 | caps::HEVC,
        resume: Some(([0x11; 8], 0x0001_0002)),
        last_fid: Some((0xFFFF, 3)),
        mode: Some((mode::COMPETITIVE, mode_flags::TAKEOVER)),
        name: Some("客户端-1".into()),
        bitrate_cap_kbps: Some(15_000),
    }
}

fn ok_resp(token: [u8; 16]) -> HsRespPayload {
    HsRespPayload {
        ver: PROTO_VERSION,
        min_ver: PROTO_MIN_VERSION,
        caps: caps::SPLIT2 | caps::HEVC,
        err: HsErr::Ok,
        reset_token: Some(token),
        session: Some(SessionInfo { ps_state: ps_state::STREAMING, host_id: [0x22; 8], epoch: 0xABCD_0001, resumable: true }),
    }
}

/// 网关一侧的允许列表 + 每个客户端的 max_ts（只在 HS_INIT 整个解开之后才更新）
struct GwModel {
    allowed: HashMap<[u8; 32], Secret32>,
    max_ts: HashMap<[u8; 32], Tai64N>,
}

impl GwModel {
    fn new(f: &Fixture) -> Self {
        Self { allowed: HashMap::from([(f.cl.public(), f.psk.clone())]), max_ts: HashMap::new() }
    }

    fn accept(&mut self, gw: &StaticKeypair, pkt: &[u8]) -> Result<HsInitAccepted, DropReason> {
        self.accept_hooks(gw, pkt, &HsTestHooks::default())
    }

    fn accept_hooks(&mut self, gw: &StaticKeypair, pkt: &[u8], hooks: &HsTestHooks) -> Result<HsInitAccepted, DropReason> {
        let allowed = &self.allowed;
        let max_ts = &self.max_ts;
        let r = ywp::crypto::noise::accept_hs_init_with_hooks(
            gw,
            &mac1_key(&gw.public()),
            pkt,
            |p| allowed.get(p).map(|psk| HsPeer { psk: psk.clone(), last_ts: max_ts.get(p).copied() }),
            hooks,
        );
        if let Ok(a) = &r {
            self.max_ts.insert(a.client_pub, a.payload.ts);
        }
        r
    }
}

fn start(f: &Fixture, attempt: u32, ts: Tai64N) -> (InitiatorAttempt, Vec<u8>) {
    let mut out = [0u8; HS_INIT_LEN];
    let (a, n) = InitiatorAttempt::start(&f.cl, &f.gw.public(), &f.psk, f.cid, attempt, &init_payload(ts), &mut out).unwrap();
    (a, out[..n].to_vec())
}

fn respond(acc: HsInitAccepted, resp: &HsRespPayload) -> (Vec<u8>, TransportKeys) {
    let mut out = [0u8; BASE_PAYLOAD];
    let (n, k) = acc.respond(resp, &mut out).unwrap();
    (out[..n].to_vec(), k)
}

fn data_hdr() -> OuterHeader {
    OuterHeader { ptype: PacketType::Data, key_phase: false, class: Class::Input, cid: fixture().cid, path_id: 0, pn_trunc: 0 }
}

fn seal_ping(k: &DirKey, pn: u64) -> Vec<u8> {
    let mut buf = [0u8; 64];
    let mut w = PacketWriter::new(&mut buf, 64, 1);
    w.push(&Ping(pn as u16)).unwrap();
    let n = w.seal(&data_hdr(), pn, k);
    buf[..n].to_vec()
}

#[test]
fn full_handshake_in_memory() {
    let f = fixture();
    let mut gw = GwModel::new(&f);
    let ts = Tai64N::from_unix(1_700_000_000, 0);
    let (mut att, init) = start(&f, 0, ts);

    // HS_INIT 正好 1232 B；mac1 在 [1200, 1216)，覆盖 [0, 1200)；mac2 全 0
    assert_eq!(init.len(), 1232);
    assert_eq!(init[0], FLAGS_HS_INIT);
    assert_eq!(OuterHeader::peek_cid(&init), Some(f.cid));
    assert_eq!(&init[10..14], &[0, 0, 0, 0], "PN = 尝试序号");
    assert_eq!(init[1200..1216], mac1(&mac1_key(&f.gw.public()), &init[..1200]));
    assert_eq!(init[1216..], [0u8; 16]);
    assert_eq!(noise::hs_init_ephemeral(&init).unwrap(), init[14..46]);

    let acc = gw.accept(&f.gw, &init).unwrap();
    assert_eq!(acc.client_pub, f.cl.public());
    assert_eq!(acc.payload, init_payload(ts));
    assert_eq!((acc.cid, acc.pn, acc.path_id), (f.cid, 0, 0));
    let token = reset_token(&[0xEE; 32], f.cid);
    let (resp, gk) = respond(acc, &ok_resp(token));
    assert_eq!(resp[0], FLAGS_HS_RESP);
    assert!(resp.len() <= init.len(), "回包不大于请求");
    let rl = resp.len();
    assert_eq!(resp[rl - 32..rl - 16], mac1(&mac1_key(&f.cl.public()), &resp[..rl - 32]), "HS_RESP 的 mac1 按客户端公钥");

    let (got, ck) = att.read_resp(&resp).unwrap();
    assert_eq!(got, ok_resp(token));
    // 两端的 k 和 iv 一致：客户端 tx = 网关 rx（c2s），客户端 rx = 网关 tx（s2c）
    assert_eq!(ck.tx.raw_for_testing(), gk.rx.raw_for_testing());
    assert_eq!(ck.rx.raw_for_testing(), gk.tx.raw_for_testing());
    // 实际收发
    let mut p = seal_ping(&ck.tx, 0);
    assert!(open_data(&mut p, &gk.rx, &mut PathRx::new()).is_ok());
    let mut p = seal_ping(&gk.tx, 0);
    assert!(open_data(&mut p, &ck.rx, &mut PathRx::new()).is_ok());
    // 方向不能混：c2s 的包用 s2c 的密钥解不开
    let mut p = seal_ping(&ck.tx, 1);
    assert_eq!(open_data(&mut p, &ck.rx, &mut PathRx::new()).unwrap_err(), DropReason::AeadFail);
}

#[test]
fn handshake_failures() {
    let f = fixture();
    let ts = Tai64N::from_unix(1_700_000_000, 0);

    // 网关公钥错：客户端算 mac1 用的是错的公钥
    {
        let mut gw = GwModel::new(&f);
        let wrong = StaticKeypair::from_secret([0x09; 32]);
        let mut out = [0u8; HS_INIT_LEN];
        InitiatorAttempt::start(&f.cl, &wrong.public(), &f.psk, f.cid, 0, &init_payload(ts), &mut out).unwrap();
        assert_eq!(gw.accept(&f.gw, &out).unwrap_err(), DropReason::Mac1Fail);
        // 知道网关公钥、mac1 对，但网关私钥不是这把（比如换过密钥）：Noise 解不开
        let (_, init) = start(&f, 0, ts);
        let other_gw = StaticKeypair::from_secret([0x0A; 32]);
        let r = noise::accept_hs_init(&other_gw, &mac1_key(&f.gw.public()), &init, |_| None);
        assert_eq!(r.unwrap_err(), DropReason::HsNoiseFail);
    }

    // 未知客户端：静默丢弃，而且不更新 max_ts
    {
        let mut gw = GwModel::new(&f);
        gw.allowed.clear();
        let (_, init) = start(&f, 0, ts);
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::UnknownClient);
        assert!(gw.max_ts.is_empty());
        // 配对之后同一个包能进（证明刚才没把 ts 记下）
        gw.allowed.insert(f.cl.public(), f.psk.clone());
        assert!(gw.accept(&f.gw, &init).is_ok());
    }

    // PSK 错：PSK 只进第二条消息，网关照常回 HS_RESP，客户端解不开
    {
        let mut gw = GwModel::new(&f);
        gw.allowed.insert(f.cl.public(), Secret32::new([0x04; 32]));
        let (mut att, init) = start(&f, 0, ts);
        let acc = gw.accept(&f.gw, &init).unwrap();
        let (resp, _) = respond(acc, &ok_resp([0; 16]));
        assert_eq!(att.read_resp(&resp).unwrap_err(), DropReason::HsNoiseFail);
    }

    // TAI64N 没有增大：同一个包重放、或更早的时间戳
    {
        let mut gw = GwModel::new(&f);
        let (_, init) = start(&f, 0, ts);
        assert!(gw.accept(&f.gw, &init).is_ok());
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::StaleTimestamp, "首包双发的第二份");
        let (_, older) = start(&f, 1, Tai64N::from_unix(1_699_999_999, 999_999_999));
        assert_eq!(gw.accept(&f.gw, &older).unwrap_err(), DropReason::StaleTimestamp);
        assert_eq!(gw.max_ts[&f.cl.public()], ts, "失败的尝试不更新 max_ts");
        let (_, newer) = start(&f, 2, ts.plus_nanos(1));
        assert!(gw.accept(&f.gw, &newer).is_ok());
    }

    // prologue 不符（CID 之外的部分被改）：mac1 对，Noise 解不开
    {
        let mut gw = GwModel::new(&f);
        let hooks = HsTestHooks { prologue: Some(b"YWP v0.2\x01\x23\x45\x67\x89\xAB\xCD\xEF".to_vec()), ..Default::default() };
        let mut out = [0u8; HS_INIT_LEN];
        InitiatorAttempt::start_with_hooks(&f.cl, &f.gw.public(), &f.psk, f.cid, 0, &init_payload(ts), &mut out, &hooks)
            .unwrap();
        assert_eq!(gw.accept(&f.gw, &out).unwrap_err(), DropReason::HsNoiseFail);
        assert!(gw.max_ts.is_empty());
    }

    // 外层头的 CID 被改：mac1 覆盖外层头，先在 mac1 上失败；重算 mac1 后 prologue 对不上
    {
        let mut gw = GwModel::new(&f);
        let (_, mut init) = start(&f, 0, ts);
        init[8] ^= 1;
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::Mac1Fail);
        let m = mac1(&mac1_key(&f.gw.public()), &init[..1200]);
        init[1200..1216].copy_from_slice(&m);
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::HsNoiseFail);
    }

    // 包长：短于 1180 一律丢；1180~1500 都接受（mac 按包尾定位）
    {
        let mut gw = GwModel::new(&f);
        let (_, init) = start(&f, 0, ts);
        assert_eq!(gw.accept(&f.gw, &init[..1179]).unwrap_err(), DropReason::HsTooShort);
        for (i, len) in [1180usize, 1300, RECV_MAX].into_iter().enumerate() {
            let hooks = HsTestHooks { init_len: Some(len), ..Default::default() };
            let mut out = vec![0u8; len];
            let t = ts.plus_nanos(i as u64 + 1);
            let (_, n) = InitiatorAttempt::start_with_hooks(&f.cl, &f.gw.public(), &f.psk, f.cid, 0, &init_payload(t), &mut out, &hooks)
                .unwrap();
            assert_eq!(n, len);
            assert!(gw.accept(&f.gw, &out).is_ok(), "L = {len}");
        }
        let mut big = init.clone();
        big.resize(RECV_MAX + 1, 0);
        assert_eq!(gw.accept(&f.gw, &big).unwrap_err(), DropReason::TooLong);
    }

    // mac2 之前的任意一个字节被改都静默丢弃；mac2 在 v1 里不校验（cookie 以后再做）
    {
        let (_, init) = start(&f, 0, ts);
        for i in (0..1216).step_by(7) {
            let mut gw = GwModel::new(&f);
            let mut p = init.clone();
            p[i] ^= 0x80;
            assert!(gw.accept(&f.gw, &p).is_err(), "第 {i} 字节");
            assert!(gw.max_ts.is_empty());
        }
        let mut gw = GwModel::new(&f);
        let mut p = init.clone();
        p[1220] = 0xFF;
        assert!(gw.accept(&f.gw, &p).is_ok());
    }

    // 握手包外层头的 K 位、Class 不为 0
    {
        let mut gw = GwModel::new(&f);
        let (_, mut init) = start(&f, 0, ts);
        init[0] = FLAGS_HS_INIT | class::VIDEO;
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::BadClass);
        init[0] = FLAGS_HS_INIT | FLAGS_K_BIT;
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::KeyPhase);
        init[0] = FLAGS_HS_RESP;
        assert_eq!(gw.accept(&f.gw, &init).unwrap_err(), DropReason::BadHeader);
    }
}

#[test]
fn hs_resp_checks_keep_attempt_state() {
    let f = fixture();
    let mut gw = GwModel::new(&f);
    let (mut att, init) = start(&f, 3, Tai64N::from_unix(5, 0));
    let acc = gw.accept(&f.gw, &init).unwrap();
    assert_eq!(acc.pn, 3);
    let (resp, gk) = respond(acc, &ok_resp([7; 16]));
    assert_eq!(&resp[10..14], &[0, 0, 0, 3], "PN 回显");

    // PN 对不上、CID 对不上：HsUnexpected；mac1 坏：Mac1Fail；密文坏：HsNoiseFail。之后正确的包照常能用
    let mut p = resp.clone();
    p[13] = 4;
    assert_eq!(att.read_resp(&p).unwrap_err(), DropReason::HsUnexpected);
    let mut p = resp.clone();
    p[1] ^= 1;
    assert_eq!(att.read_resp(&p).unwrap_err(), DropReason::HsUnexpected);
    let mut p = resp.clone();
    let l = p.len();
    p[l - 20] ^= 1;
    assert_eq!(att.read_resp(&p).unwrap_err(), DropReason::Mac1Fail);
    let mut p = resp.clone();
    p[20] ^= 1;
    let m = mac1(&mac1_key(&f.cl.public()), &p[..l - 32]);
    p[l - 32..l - 16].copy_from_slice(&m);
    assert_eq!(att.read_resp(&p).unwrap_err(), DropReason::HsNoiseFail);
    assert_eq!(att.read_resp(&resp[..60]).unwrap_err(), DropReason::TooShort);

    let (_, ck) = att.read_resp(&resp).unwrap();
    assert_eq!(ck.tx.raw_for_testing(), gk.rx.raw_for_testing());
}

#[test]
fn late_first_resp_still_usable() {
    // 同一 CID 的两次尝试（PN 0、1），网关两份都回、都留作候选；客户端用迟到的第一份派生密钥，
    // 网关收到第一个 DATA 时逐份试解：第二份候选失败（缓冲不变），第一份解开
    let f = fixture();
    let mut gw = GwModel::new(&f);
    let t0 = Tai64N::from_unix(100, 0);
    let (mut a0, i0) = start(&f, 0, t0);
    let (mut a1, i1) = start(&f, 1, t0.plus_micros(250_000));
    assert_eq!(OuterHeader::peek_cid(&i0), OuterHeader::peek_cid(&i1));
    let acc0 = gw.accept(&f.gw, &i0).unwrap();
    let acc1 = gw.accept(&f.gw, &i1).unwrap();
    let (r0, g0) = respond(acc0, &ok_resp([1; 16]));
    let (r1, g1) = respond(acc1, &ok_resp([1; 16]));
    // 回包按 PN 找尝试：r1 交给 a0 不行
    assert_eq!(a0.read_resp(&r1).unwrap_err(), DropReason::HsUnexpected);
    let (_, c0) = a0.read_resp(&r0).unwrap();
    let _ = a1.read_resp(&r1).unwrap();

    let mut candidates = [(g1, PathRx::new()), (g0, PathRx::new())];
    let mut pkt = seal_ping(&c0.tx, 0);
    let original = pkt.clone();
    let mut hit = None;
    for (i, (keys, rx)) in candidates.iter_mut().enumerate() {
        let mut trial = pkt.clone();
        match open_data(&mut trial, &keys.rx, rx) {
            Ok(_) => {
                hit = Some(i);
                break;
            }
            Err(e) => {
                assert_eq!(e, DropReason::AeadFail);
                // 原地试解失败也不改缓冲
                assert_eq!(open_data(&mut pkt, &keys.rx, &mut PathRx::new()).unwrap_err(), DropReason::AeadFail);
                assert_eq!(pkt, original);
            }
        }
    }
    assert_eq!(hit, Some(1), "第一次尝试的候选解开");
}

#[test]
fn reset_rules() {
    let f = fixture();
    let reset_key = [0x5C; 32];
    let token = reset_token(&reset_key, f.cid);
    let mut out = [0u8; 64];
    let n = encode_reset(f.cid, 0, &token, &mut out);
    assert_eq!(n, RESET_LEN);
    assert_eq!(out[0], FLAGS_RESET);
    let pkt = out[..n].to_vec();
    assert_eq!(check_reset(&pkt, f.cid, &token), Ok(()));
    // 网关丢了状态也能算出同一个令牌
    assert_eq!(check_reset(&pkt, f.cid, &reset_token(&reset_key, f.cid)), Ok(()));
    // 只认正好 38 B
    assert_eq!(check_reset(&pkt[..37], f.cid, &token), Err(DropReason::ResetBadLen));
    let mut longer = pkt.clone();
    longer.push(0);
    assert_eq!(check_reset(&longer, f.cid, &token), Err(DropReason::ResetBadLen));
    // 只认当前 CID
    assert_eq!(check_reset(&pkt, Cid(f.cid.0 ^ 1), &token), Err(DropReason::ResetBadCid));
    // 令牌不一致直接忽略
    let other = reset_token(&[0x5D; 32], f.cid);
    assert_eq!(check_reset(&pkt, f.cid, &other), Err(DropReason::ResetBadToken));
    let mut bad = pkt.clone();
    bad[37] ^= 1;
    assert_eq!(check_reset(&bad, f.cid, &token), Err(DropReason::ResetBadToken));
    // Flags 不是 0x70
    let mut bad = pkt.clone();
    bad[0] = 0x40;
    assert_eq!(check_reset(&bad, f.cid, &token), Err(DropReason::BadHeader));
    // PN 和 8 B 随机部分每次不同
    let mut out2 = [0u8; 64];
    encode_reset(f.cid, 0, &token, &mut out2);
    assert_ne!(out[10..22], out2[10..22]);
}

#[test]
fn busy_response() {
    let f = fixture();
    let mut gw = GwModel::new(&f);
    let (mut att, init) = start(&f, 0, Tai64N::from_unix(1, 0));
    let acc = gw.accept(&f.gw, &init).unwrap();
    let busy = HsRespPayload { err: HsErr::Busy, reset_token: None, session: None, ..ok_resp([0; 16]) };
    let (resp, _) = respond(acc, &busy);
    let (got, _) = att.read_resp(&resp).unwrap();
    assert_eq!(got.err, HsErr::Busy);
    // err = OK 却没有 reset_token：编码就拒绝
    let (_, init) = start(&f, 1, Tai64N::from_unix(2, 0));
    let acc = gw.accept(&f.gw, &init).unwrap();
    let bad = HsRespPayload { reset_token: None, ..ok_resp([0; 16]) };
    let mut out = [0u8; 256];
    assert!(acc.respond(&bad, &mut out).is_err());
}

// ---------------------------------------------------------------- 金标向量

fn golden() -> String {
    let f = fixture();
    let reset_key = [0x5C; 32];
    let e_c = [0x0C; 32];
    let e_g = [0x0D; 32];
    let ts = Tai64N::from_unix(1_700_000_000, 123_456_789);
    let mut s = String::new();
    let mut line = |k: &str, v: &[u8]| writeln!(s, "{k} = {}", to_hex(v)).unwrap();
    line("gw_static_secret", &[0x01; 32]);
    line("gw_static_public", &f.gw.public());
    line("client_static_secret", &[0x02; 32]);
    line("client_static_public", &f.cl.public());
    line("psk", f.psk.expose());
    line("reset_key", &reset_key);
    line("cid", &f.cid.0.to_be_bytes());
    line("ts", &ts.to_bytes());
    line("client_ephemeral_secret", &e_c);
    line("gw_ephemeral_secret", &e_g);

    let hooks_c = HsTestHooks { fixed_ephemeral: Some(e_c), ..Default::default() };
    let hooks_g = HsTestHooks { fixed_ephemeral: Some(e_g), ..Default::default() };
    let mut init = [0u8; HS_INIT_LEN];
    let (mut att, n) =
        InitiatorAttempt::start_with_hooks(&f.cl, &f.gw.public(), &f.psk, f.cid, 0, &init_payload(ts), &mut init, &hooks_c)
            .unwrap();
    line("hs_init", &init[..n]);
    let mut gw = GwModel::new(&f);
    let acc = gw.accept_hooks(&f.gw, &init[..n], &hooks_g).unwrap();
    let token = reset_token(&reset_key, f.cid);
    let (resp, gk) = respond(acc, &ok_resp(token));
    line("hs_resp", &resp);
    let (_, ck) = att.read_resp(&resp).unwrap();
    let (k_c2s, iv_c2s) = ck.tx.raw_for_testing();
    let (k_s2c, iv_s2c) = ck.rx.raw_for_testing();
    line("k_c2s", &k_c2s);
    line("iv_c2s", &iv_c2s);
    line("k_s2c", &k_s2c);
    line("iv_s2c", &iv_s2c);

    // c2s：PING + INPUT（PN 0）
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut w = PacketWriter::new(&mut buf, UPLINK_MAX, 0x0001_0000);
    w.push(&Ping(1)).unwrap();
    w.push(&InputChunkOwned {
        in_seq: 1,
        connected: true,
        zero: false,
        state: InputState::default(),
        n_ev: 1,
        events: {
            let mut e = [InputEvent::default(); INPUT_MAX_EVENTS];
            e[0] = InputEvent { ev_seq: 1, code: button::CROSS, value: 1, dt_100us: 2 };
            e
        },
    })
    .unwrap();
    let n = w.seal(&OuterHeader { class: Class::Input, ..data_hdr() }, 0, &ck.tx);
    line("data_c2s_pn0", &buf[..n]);
    let mut p = buf[..n].to_vec();
    open_data(&mut p, &gk.rx, &mut PathRx::new()).expect("网关打开 c2s");

    // s2c：PONG + VIDEO 源符号（PN 0x1_0000_0002：PN 高位进 nonce）
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut w = PacketWriter::new(&mut buf, BASE_PAYLOAD, 0x0001_0100);
    w.push(&Pong { ping_id: 1, echo_ts: 0x0001_0000, hold_10us: 5 }).unwrap();
    let vh = VideoHeader {
        key: true,
        parity: false,
        last_src: true,
        repair: false,
        fid: 0,
        sym_idx: 0,
        k: 1,
        r: 0,
        age_100us: 40,
        profile: 0,
        n: 1,
        cfg: 1,
        w: false,
    };
    w.push_video_tail(&vh, b"\x00\x00\x00\x01\x26\x01").unwrap();
    let pn = (1u64 << 32) | 2;
    let n = w.seal(&OuterHeader { class: Class::Video, ..data_hdr() }, pn, &gk.tx);
    line("data_s2c_pn100000002", &buf[..n]);
    let mut p = buf[..n].to_vec();
    let mut rx = PathRx { largest: Some((1 << 32) - 5), ..PathRx::new() };
    assert_eq!(open_data(&mut p, &ck.rx, &mut rx).expect("客户端打开 s2c").pn, pn);

    let mut out = [0u8; RESET_LEN];
    let mut rand = [0u8; 12];
    rand[..4].copy_from_slice(&0xA1B2_C3D4u32.to_be_bytes());
    rand[4..].copy_from_slice(&[0xEE; 8]);
    encode_reset_with(f.cid, 0, &token, &rand, &mut out);
    line("reset", &out);
    s
}

#[test]
fn golden_vectors() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/golden.txt");
    let now = golden();
    // 同样的输入两次结果一样（固定了两端的临时密钥）
    assert_eq!(now, golden());
    if std::env::var_os("YWP_REGEN_VECTORS").is_some() {
        std::fs::write(&path, format!("{HEADER}{now}")).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).expect("缺 tests/vectors/golden.txt；用 YWP_REGEN_VECTORS=1 生成");
    let want: String = want.lines().filter(|l| !l.starts_with('#') && !l.is_empty()).map(|l| format!("{l}\n")).collect();
    for (a, b) in want.lines().zip(now.lines()) {
        assert_eq!(a, b, "金标向量变了：线上格式或密钥派生被改动");
    }
    assert_eq!(want.lines().count(), now.lines().count());
}

const HEADER: &str = "\
# YWP v0.2 金标向量（由 tests/handshake.rs 生成，改线上格式时才重新生成）
# 全部十六进制。密钥、PSK、CID、时间戳、两端临时私钥都是固定的测试值，不是真实凭据。
# hs_init：客户端第 0 次尝试；hs_resp：网关的回应（err = 0，带 reset_token 和 0x12）；
# k_* / iv_*：Split() 和 HKDF 派生的方向密钥与 iv；data_*：两个方向各一个 DATA 包；reset：38 B 的 RESET。
";
