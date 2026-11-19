//! 线上格式的离线测试（A 组）：外层头、块解析两遍、各块编解码、PN 还原、重放窗口、包长（规格 §5.6）。

use proptest::prelude::*;
use std::collections::HashSet;
use ywp::consts::*;
use ywp::crypto::DirKey;
use ywp::packet::{open_data, Full, PacketWriter, PathRx};
use ywp::pn::decode_pn;
use ywp::replay::ReplayWindow;
use ywp::wire::*;
use ywp::{DropReason, WireError};

fn key() -> DirKey {
    DirKey::new([0x11; 32], [0x22; 12])
}

fn data_hdr(class: Class) -> OuterHeader {
    OuterHeader { ptype: PacketType::Data, key_phase: false, class, cid: Cid(0xC1D0_C1D0_C1D0_C1D0), path_id: 0, pn_trunc: 0 }
}

/// 用 PacketWriter 组一个包、用 open_data 打开，返回块明文
fn roundtrip(f: impl FnOnce(&mut PacketWriter<'_>)) -> (Vec<u8>, usize) {
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut w = PacketWriter::new(&mut buf, BASE_PAYLOAD, 0xDEAD_BEEF);
    f(&mut w);
    let k = key();
    let n = w.seal(&data_hdr(Class::Media), 7, &k);
    let mut pkt = buf[..n].to_vec();
    let mut rx = PathRx::new();
    let o = open_data(&mut pkt, &k, &mut rx).expect("打开");
    assert_eq!(o.pn, 7);
    assert_eq!(o.send_ts, 0xDEAD_BEEF);
    (o.chunks.to_vec(), n)
}

fn chunks(plain: &[u8]) -> Vec<Result<Chunk<'_>, ChunkError>> {
    validate_chunks(plain).expect("第一遍");
    ChunkIter::new(plain).collect()
}

// ---------------------------------------------------------------- 外层头

#[test]
fn header_flags_and_bad_versions() {
    let cid = Cid(0x0102_0304_0506_0708);
    for (t, flags) in [(PacketType::HsInit, FLAGS_HS_INIT), (PacketType::HsResp, FLAGS_HS_RESP), (PacketType::Reset, FLAGS_RESET)] {
        let mut b = [0u8; HDR_LEN];
        OuterHeader::new(t, cid, 0, 0).encode(&mut b);
        assert_eq!(b[0], flags);
    }
    for (class, flags) in [(Class::Bulk, 0x40), (Class::Video, 0x41), (Class::Media, 0x42), (Class::Input, 0x43)] {
        let mut b = [0u8; HDR_LEN];
        data_hdr(class).encode(&mut b);
        assert_eq!(b[0], flags);
        assert_eq!(OuterHeader::decode(&b).unwrap().class, class);
    }
    let mut b = [0u8; HDR_LEN];
    data_hdr(Class::Video).encode(&mut b);
    for ver in [0b00u8, 0b10, 0b11] {
        let mut x = b;
        x[0] = (x[0] & 0x3F) | (ver << 6);
        assert_eq!(OuterHeader::decode(&x), Err(WireError::BadVersion), "Ver = {ver:02b}");
    }
    let mut x = b;
    x[0] = 0x40 | (7 << 3);
    assert_eq!(OuterHeader::decode(&x), Err(WireError::ReservedType));
    assert_eq!(OuterHeader::decode(&b[..13]), Err(WireError::Short));
    assert_eq!(OuterHeader::peek_cid(&b[..13]), None);
    // K 位
    x = b;
    x[0] |= FLAGS_K_BIT;
    assert!(OuterHeader::decode(&x).unwrap().key_phase);
}

proptest! {
    #[test]
    fn header_any_valid_bytes_roundtrip(mut b in proptest::array::uniform14(any::<u8>())) {
        b[0] = (b[0] & 0x3F) | 0x40;
        match OuterHeader::decode(&b) {
            Ok(h) => {
                let mut out = [0u8; HDR_LEN];
                h.encode(&mut out);
                prop_assert_eq!(out, b);
                prop_assert_eq!(OuterHeader::peek_cid(&b), Some(h.cid));
            }
            Err(e) => prop_assert_eq!(e, WireError::ReservedType),
        }
    }

    #[test]
    fn parsers_never_panic(data in proptest::collection::vec(any::<u8>(), 0..1600)) {
        let _ = OuterHeader::decode(&data);
        if validate_chunks(&data).is_ok() {
            for c in ChunkIter::new(&data) {
                let _ = c;
            }
        }
        // 没经过第一遍也不能 panic
        for c in ChunkIter::new(&data) {
            let _ = c;
        }
        let _ = HsInitPayload::decode(&data);
        let _ = HsRespPayload::decode(&data);
        let mut pkt = data.clone();
        let mut rx = PathRx::new();
        prop_assert!(open_data(&mut pkt, &key(), &mut rx).is_err());
        prop_assert!(rx.largest.is_none());
    }
}

// ---------------------------------------------------------------- 块

#[test]
fn every_chunk_roundtrips() {
    let input = InputChunkOwned {
        in_seq: 0xBEEF,
        connected: true,
        zero: false,
        state: InputState {
            buttons: 0x8001,
            l2: 7,
            r2: 255,
            sticks: [-32768, 32767, -1, 0],
            touch: [Touch { id: 5, x: 0xFFF, y: 0x123 }, Touch::NONE],
            gyro: [1, 2, 3],
            accel: [65535, 0, 32768],
            orient: 0,
            flags: INPUT_STATE_FLAG_MOTION | INPUT_STATE_FLAG_TOUCH,
        },
        n_ev: 2,
        events: {
            let mut e = [InputEvent::default(); INPUT_MAX_EVENTS];
            e[0] = InputEvent { ev_seq: 9, code: ev_code::L2, value: 200, dt_100us: 0xFFFF };
            e[1] = InputEvent { ev_seq: 8, code: button::CROSS, value: 1, dt_100us: 3 };
            e
        },
    };
    let fb = FeedbackState {
        fb_seq: 3,
        valid: fb_valid::RUMBLE | fb_valid::LED,
        rumble: [1, 2],
        trig_l: [3; 11],
        trig_r: [4; 11],
        rgb: [5, 6, 7],
        player: 8,
        intensity: 0x9A,
    };
    let report = ReportOwned {
        rep_seq: 77,
        video: ReportVideoFields {
            mode: mode::STABLE,
            sb90_ms: 12,
            j_ms: 30,
            frames_ok: 1,
            frames_late: 2,
            frames_repair: 3,
            frames_lost: 4,
            fec_recovered: 5,
            oldest_needed_fid: 6,
            blocked_fid: BLOCKED_FID_NONE,
            blocked_need: 0,
        },
        paths: vec![PathReport { path_id: 0, largest_pn: 0xFFFF_FFFF, recv_cnt: 10, lost_cnt: 1, lost_short: 1, burst_max: 1, ..Default::default() }],
    };
    let up = UpReportOwned { paths: vec![UpPathReport { path_id: 0, in_recv: 5, in_lost: 1, q_up: 0, up_loss_ppm: 0 }; 2] };
    let fs = FrameStatusOwned {
        entries: vec![
            FrameStatusEntry { fid: 0xFFFF, kind: FrameStatusKind::KeyPending, arg: 0 },
            FrameStatusEntry { fid: 3, kind: FrameStatusKind::Gone, arg: 9 },
        ],
    };
    let ctl = ControlFragOwned { stream: stream::CONFIG, msg_seq: 0xFFFF, frag: 0, data: vec![msg_ty::VIDEO_CONFIG, 1, 0, 1, 7] };
    let audio = AudioChunkOwned::new(10, 25, vec![vec![0xAA; 160], vec![]]);
    let (plain, _) = roundtrip(|w| {
        w.push(&Ping(0x1234)).unwrap();
        w.push(&Pong { ping_id: 1, echo_ts: 2, hold_10us: 3 }).unwrap();
        w.push(&PathChallenge([1; 8])).unwrap();
        w.push(&PathResponse([2; 8])).unwrap();
        w.push(&CloseOwned { code: close_code::GATEWAY_EXIT, reason: "退出".repeat(30) }).unwrap();
        w.push(&ctl).unwrap();
        w.push(&Ack { stream: 1, cum: CTL_CUM_INIT, sack: 0x8000_0001 }).unwrap();
        w.push(&audio).unwrap();
        w.push(&input).unwrap();
        w.push(&fb).unwrap();
        w.push(&report).unwrap();
        w.push(&NackOwned { path_id: 0, segs: vec![(0xFFFF_FFF0, 255), (3, 1)] }).unwrap();
        w.push(&Need { fid: 4, need: NEED_UNKNOWN_K, key: true, tries: 3 }).unwrap();
        w.push(&fs).unwrap();
        w.push(&up).unwrap();
    });
    let got = chunks(&plain);
    assert_eq!(got.len(), 15);
    let mut it = got.into_iter().map(|c| c.expect("块"));
    assert!(matches!(it.next(), Some(Chunk::Ping(0x1234))));
    assert!(matches!(it.next(), Some(Chunk::Pong(Pong { ping_id: 1, echo_ts: 2, hold_10us: 3 }))));
    assert!(matches!(it.next(), Some(Chunk::PathChallenge(c)) if c == [1; 8]));
    assert!(matches!(it.next(), Some(Chunk::PathResponse(c)) if c == [2; 8]));
    match it.next() {
        Some(Chunk::Close { code, reason }) => {
            assert_eq!(code, close_code::GATEWAY_EXIT);
            assert!(reason.len() <= chunk_len::CLOSE_REASON_MAX);
            assert!(std::str::from_utf8(reason).is_ok(), "按字符边界截断");
        }
        c => panic!("{c:?}"),
    }
    match it.next() {
        Some(Chunk::Control(f)) => {
            assert_eq!((f.stream, f.msg_seq, f.frag, f.data), (ctl.stream, ctl.msg_seq, 0, &ctl.data[..]));
            assert_eq!((f.frag_index(), f.frag_count()), (0, 1));
        }
        c => panic!("{c:?}"),
    }
    assert!(matches!(it.next(), Some(Chunk::Ack(Ack { stream: 1, cum: 0xFFFF, sack: 0x8000_0001 }))));
    match it.next() {
        Some(Chunk::Audio(a)) => {
            assert_eq!((a.aseq, a.age_100us, a.cnt), (10, 25, 2));
            let f: Vec<_> = a.frames().collect();
            assert_eq!(f, vec![(10, &[0xAA; 160][..]), (9, &[][..])]);
        }
        c => panic!("{c:?}"),
    }
    match it.next() {
        Some(Chunk::Input(i)) => {
            assert_eq!((i.in_seq, i.connected, i.zero, i.state), (input.in_seq, true, false, input.state));
            assert_eq!(i.events().collect::<Vec<_>>(), input.events[..2].to_vec());
        }
        c => panic!("{c:?}"),
    }
    assert!(matches!(it.next(), Some(Chunk::Feedback(f)) if f == fb));
    match it.next() {
        Some(Chunk::Report(r)) => {
            assert_eq!((r.rep_seq, r.n_paths, r.video), (77, 1, report.video));
            assert_eq!(r.paths().collect::<Vec<_>>(), report.paths);
        }
        c => panic!("{c:?}"),
    }
    match it.next() {
        Some(Chunk::Nack(n)) => assert_eq!(n.segments().collect::<Vec<_>>(), vec![(0xFFFF_FFF0, 255), (3, 1)]),
        c => panic!("{c:?}"),
    }
    assert!(matches!(it.next(), Some(Chunk::Need(Need { fid: 4, need: 0xFF, key: true, tries: 3 }))));
    match it.next() {
        Some(Chunk::FrameStatus(l)) => {
            assert_eq!(l.len(), 2);
            assert_eq!(l.iter().collect::<Vec<_>>(), fs.entries);
            assert_eq!(l.bad_kinds(), 0);
        }
        c => panic!("{c:?}"),
    }
    match it.next() {
        Some(Chunk::UpReport(u)) => assert_eq!(u.paths().collect::<Vec<_>>(), up.paths),
        c => panic!("{c:?}"),
    }
}

#[test]
fn reserved_chunk_drops_whole_packet() {
    // PING 在前、保留块 0x05 在后：整包丢，前面的 PING 也不生效（open_data 不交出任何块）
    let mut plain = vec![chunk_ty::PING, 0, 2, 0, 1, 0x05, 0, 0];
    assert_eq!(validate_chunks(&plain).unwrap_err(), WireError::ReservedType);
    for ty in RESERVED_FIRST_LAST() {
        plain[5] = ty;
        assert_eq!(validate_chunks(&plain).unwrap_err(), WireError::ReservedType);
    }
    // 经 open_data：AEAD 通过（PN 照样提交），但整包按 ReservedChunk 丢弃，不交出前面的 PING
    let k = key();
    let mut rx = PathRx::new();
    let mut pkt = seal_raw(&[chunk_ty::PING, 0, 2, 0, 1, 0x01], 0, &k);
    assert_eq!(open_data(&mut pkt, &k, &mut rx).unwrap_err(), DropReason::ReservedChunk);
    let mut pkt = seal_raw(&[chunk_ty::PING, 0, 9, 0, 1], 1, &k);
    assert_eq!(open_data(&mut pkt, &k, &mut rx).unwrap_err(), DropReason::MalformedChunk);
    assert_eq!(rx.largest, Some(1));
}

/// 按给定的块明文手工封一个 DATA 包（不经 PacketWriter，可以放任意字节）
fn seal_raw(chunks: &[u8], pn: u64, k: &DirKey) -> Vec<u8> {
    let mut hdr = data_hdr(Class::Bulk);
    hdr.pn_trunc = pn as u32;
    let mut pkt = vec![0u8; HDR_LEN];
    hdr.encode(&mut pkt);
    let mut body = vec![0u8; SEND_TS_LEN];
    body.extend_from_slice(chunks);
    let tag = k.seal(hdr.path_id, pn, &pkt[..HDR_LEN], &mut body);
    pkt.extend_from_slice(&body);
    pkt.extend_from_slice(&tag);
    pkt
}

#[allow(non_snake_case)]
fn RESERVED_FIRST_LAST() -> std::ops::RangeInclusive<u8> {
    chunk_ty::RESERVED_FIRST..=chunk_ty::RESERVED_LAST
}

#[test]
fn truncated_and_unknown_chunks() {
    // len 越界：整包丢
    assert_eq!(validate_chunks(&[chunk_ty::PING, 0, 3, 0, 1]).unwrap_err(), WireError::Truncated);
    assert_eq!(validate_chunks(&[chunk_ty::PING, 0]).unwrap_err(), WireError::Truncated);
    // 未知块（≥ 0x20）按 len 跳过，后面的块照常
    let plain = [0x7Eu8, 0, 2, 9, 9, chunk_ty::PING, 0, 2, 0, 5];
    let s = validate_chunks(&plain).unwrap();
    assert_eq!((s.count, s.unknown, s.video), (2, 1, false));
    let got = chunks(&plain);
    assert!(matches!(got[0], Ok(Chunk::Unknown { ty: 0x7E })));
    assert!(matches!(got[1], Ok(Chunk::Ping(5))));
    // 已知块比定义短：只丢这一块，计 ShortChunk；比定义长：只读定义的部分
    let plain = [chunk_ty::PONG, 0, 3, 1, 2, 3, chunk_ty::PING, 0, 4, 0, 6, 0xFF, 0xFF];
    let got = chunks(&plain);
    assert_eq!(got[0].unwrap_err().reason, DropReason::ShortChunk);
    assert!(matches!(got[1], Ok(Chunk::Ping(6))));
    // PADDING 到包尾
    let plain = [chunk_ty::PING, 0, 2, 0, 1, 0, 0, 0, 0];
    let s = validate_chunks(&plain).unwrap();
    assert_eq!(s.count, 2);
    let got = chunks(&plain);
    assert!(matches!(got[1], Ok(Chunk::Padding)));
    assert_eq!(got.len(), 2);
    // 空明文是合法的（只有 SendTS 的包）
    assert_eq!(validate_chunks(&[]).unwrap().count, 0);
}

#[test]
fn chunk_field_checks() {
    let audio = |cnt: u8| [0u8, 1, 0, 2, cnt];
    assert_eq!(AudioChunk::parse(&audio(0)).unwrap_err(), DropReason::AudioBadCount);
    assert_eq!(AudioChunk::parse(&audio(5)).unwrap_err(), DropReason::AudioBadCount);
    assert_eq!(AudioChunk::parse(&[0, 1, 0, 2, 1, 0, 5, 1]).unwrap_err(), DropReason::ShortChunk);
    let mut inp = vec![0u8; chunk_len::INPUT_FIXED];
    inp[2] = 9;
    assert_eq!(InputChunk::parse(&inp).unwrap_err(), DropReason::InputTooManyEvents);
    inp[2] = 1;
    assert_eq!(InputChunk::parse(&inp).unwrap_err(), DropReason::ShortChunk);
    assert_eq!(Nack::parse(&[0, 0]).unwrap_err(), DropReason::NackBadSegments);
    assert_eq!(Nack::parse(&[0, 17]).unwrap_err(), DropReason::NackBadSegments);
    assert_eq!(Nack::parse(&[0, 1, 0, 0, 0, 1, 0]).unwrap_err(), DropReason::NackBadSegments);
    assert_eq!(Nack::parse(&[0, 2, 0, 0, 0, 1, 1]).unwrap_err(), DropReason::ShortChunk);
    assert_eq!(Report::parse(&[0; 19]).unwrap_err(), DropReason::ShortChunk);
    let mut rep = [0u8; 20];
    rep[2] = 1;
    assert_eq!(Report::parse(&rep).unwrap_err(), DropReason::ShortChunk);
    // 不认识的 FRAME_STATUS status 跳过并可计数
    let l = FrameStatusList::parse(&[2, 0, 1, 9, 0, 0, 2, 3, 0]).unwrap();
    assert_eq!(l.bad_kinds(), 1);
    assert_eq!(l.iter().collect::<Vec<_>>(), vec![FrameStatusEntry { fid: 2, kind: FrameStatusKind::Gone, arg: 0 }]);
    // 编码侧：内容不合法时 push 返回 Full、什么也不写
    let mut buf = [0u8; 200];
    let mut w = PacketWriter::new(&mut buf, 200, 0);
    assert_eq!(w.push(&NackOwned { path_id: 0, segs: vec![(1, 1); 17] }), Err(Full));
    assert_eq!(w.push(&AudioChunkOwned::new(0, 0, vec![])), Err(Full));
    assert!(w.is_empty());
}

fn vhdr(parity: bool, sym_idx: u16, k: u16, r: u16) -> VideoHeader {
    VideoHeader {
        key: true,
        parity,
        last_src: !parity,
        repair: false,
        fid: 0xFFFE,
        sym_idx,
        k,
        r,
        age_100us: 123,
        profile: 0,
        n: 1,
        cfg: 3,
        w: true,
    }
}

#[test]
fn video_chunks() {
    let src = vhdr(false, 4, 5, 0);
    let (plain, _) = roundtrip(|w| {
        w.push(&Pong { ping_id: 1, echo_ts: 0, hold_10us: 0 }).unwrap();
        w.push_video_tail(&src, b"nal bytes").unwrap();
        assert_eq!(w.push(&Ping(1)), Err(Full), "VIDEO 之后不能再放");
        assert_eq!(w.remaining(), 0);
    });
    assert!(validate_chunks(&plain).unwrap().video);
    let got = chunks(&plain);
    match got[1] {
        Ok(Chunk::Video(v)) => {
            assert_eq!(v.hdr, src);
            assert_eq!(v.body, b"nal bytes");
        }
        ref c => panic!("{c:?}"),
    }
    let par = vhdr(true, 7, 5, 3);
    let (plain, _) = roundtrip(|w| w.push_video_tail(&par, &[0x55; 10]).unwrap());
    match chunks(&plain)[0] {
        Ok(Chunk::Video(v)) => assert_eq!((v.hdr, v.body.len()), (par, 10)),
        ref c => panic!("{c:?}"),
    }
    // 收端校验：P 位和 sym_idx ≥ k 不一致、S 为奇数、源包 len 不到包尾、sym_idx 越界、k 越界
    let enc = |h: &VideoHeader, body: &[u8], extra: &[u8]| {
        let mut b = vec![0u8; 64];
        let mut w = Writer::new(&mut b);
        h.encode(&mut w).unwrap();
        if !h.parity {
            w.put_u16(body.len() as u16).unwrap();
        }
        w.put_slice(body).unwrap();
        w.put_slice(extra).unwrap();
        let n = w.pos();
        b.truncate(n);
        b
    };
    let parse = |b: &[u8]| VideoChunk::parse(b[0], &b[1..]).map(|_| ());
    let mut bad = enc(&vhdr(false, 5, 5, 0), b"x", &[]);
    assert_eq!(parse(&bad), Err(DropReason::VideoParityMismatch));
    bad = enc(&vhdr(true, 2, 5, 3), &[0; 4], &[]);
    assert_eq!(parse(&bad), Err(DropReason::VideoParityMismatch));
    bad = enc(&vhdr(true, 6, 5, 3), &[0; 5], &[]);
    assert_eq!(parse(&bad), Err(DropReason::VideoOddShard));
    bad = enc(&vhdr(false, 0, 5, 0), b"abc", b"z");
    assert_eq!(parse(&bad), Err(DropReason::VideoBadLen));
    bad = enc(&vhdr(false, 0, 5, 0), b"abc", &[]);
    bad.pop();
    assert_eq!(parse(&bad), Err(DropReason::VideoBadLen));
    bad = enc(&vhdr(true, 8, 5, 3), &[0; 4], &[]);
    assert_eq!(parse(&bad), Err(DropReason::VideoBadFec), "sym_idx ≥ k + R");
    // k ≤ 1024、R ≤ 1023，k + R ≤ 2048 由位宽保证；最大的帧里最后一个校验符号合法，再往后越界
    assert_eq!(parse(&enc(&vhdr(true, 2046, 1024, 1023), &[0; 4], &[])), Ok(()));
    bad = enc(&vhdr(true, 2047, 1024, 1023), &[0; 4], &[]);
    assert_eq!(parse(&bad), Err(DropReason::VideoBadFec), "sym_idx = k + R");
    let mut k2048 = enc(&vhdr(false, 0, 1, 0), b"a", &[]);
    // FEC 字在偏移 3..7；k − 1 占位 20..10，即第 4 字节低 5 位和第 5 字节高 6 位。全 1 → k = 2048
    k2048[4] |= 0x1F;
    k2048[5] |= 0xFC;
    assert_eq!(parse(&k2048), Err(DropReason::VideoBadFec));
    assert_eq!(parse(&[0x10, 0, 0]), Err(DropReason::VideoBadLen));
}

// ---------------------------------------------------------------- 包长（规格 §5.6）

fn sealed_len(f: impl FnOnce(&mut PacketWriter<'_>)) -> usize {
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut w = PacketWriter::new(&mut buf, BASE_PAYLOAD, 0);
    f(&mut w);
    w.seal(&data_hdr(Class::Video), 0, &key())
}

#[test]
fn packet_lengths_table() {
    // 满源符号：14 + 4 + 10 + 2 + 1186 + 16 = 1232
    assert_eq!(MAX_SYMBOL_DATA, 1186);
    assert_eq!(sealed_len(|w| w.push_video_tail(&vhdr(false, 0, 1, 0), &[0; MAX_SYMBOL_DATA]).unwrap()), 1232);
    assert_eq!(
        sealed_len(|w| assert_eq!(w.push_video_tail(&vhdr(false, 0, 1, 0), &[0; MAX_SYMBOL_DATA + 1]), Err(Full))),
        DATA_OVERHEAD
    );
    // 满校验：14 + 4 + 10 + 1188 + 16 = 1232
    assert_eq!(sealed_len(|w| w.push_video_tail(&vhdr(true, 1, 1, 1), &[0; MAX_SHARD_LEN]).unwrap()), 1232);
    // 音频 2 帧（每帧 160 B）：14 + 4 + 3 + 5 + 2 × 162 + 16 = 366
    let audio = AudioChunkOwned::new(1, 0, vec![vec![0; 160], vec![0; 160]]);
    assert_eq!(sealed_len(|w| w.push(&audio).unwrap()), 366);
    // 输入（8 个事件）：14 + 4 + 3 + 43 + 48 + 16 = 128
    let input = InputChunkOwned {
        in_seq: 0,
        connected: true,
        zero: false,
        state: InputState::default(),
        n_ev: 8,
        events: [InputEvent::default(); INPUT_MAX_EVENTS],
    };
    assert_eq!(sealed_len(|w| w.push(&input).unwrap()), 128);
    // 输入 + REPORT（1 条路径）+ NACK（1 段）：128 + 45 + 10 = 183
    let report = ReportOwned { paths: vec![PathReport::default()], ..Default::default() };
    let nack = NackOwned { path_id: 0, segs: vec![(1, 1)] };
    assert_eq!(
        sealed_len(|w| {
            w.push(&input).unwrap();
            w.push(&report).unwrap();
            w.push(&nack).unwrap();
        }),
        183
    );
    // 单独一个 PING：14 + 4 + 5 + 16 = 39
    assert_eq!(sealed_len(|w| w.push(&Ping(0)).unwrap()), 39);
    // 上行包限 400 B：放不下就 Full
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut w = PacketWriter::new(&mut buf, UPLINK_MAX, 0);
    assert_eq!(w.remaining(), UPLINK_MAX - DATA_OVERHEAD);
    w.push(&ControlFragOwned { stream: 0, msg_seq: 0, frag: 0, data: vec![0; UPLINK_MAX - DATA_OVERHEAD - 7] }).unwrap();
    assert_eq!(w.remaining(), 0);
    assert_eq!(w.push(&Ping(0)), Err(Full));
}

// ---------------------------------------------------------------- PN

#[test]
fn pn_wrap_and_reorder() {
    assert_eq!(decode_pn(None, 0), 0);
    assert_eq!(decode_pn(Some(0xFFFF_FFFF), 0), 1 << 32);
    assert_eq!(decode_pn(Some((1 << 32) + 3), 0xFFFF_FFFE), 0xFFFF_FFFE, "回绕后迟到的旧包");
    assert_eq!(decode_pn(Some(10), 3), 3, "乱序");
    assert_eq!(decode_pn(Some(10), 0x8000_000A), 0x8000_000A, "正好 +2³¹");
    assert_eq!(decode_pn(Some(0x9000_0000), 0x1000_0000), 0x1_1000_0000);
}

proptest! {
    #[test]
    fn pn_recovers_within_half_window(largest in 0u64..(1 << 40), delta in -(1i64 << 31) + 1..(1i64 << 31)) {
        let pn = largest as i64 + 1 + delta;
        prop_assume!(pn >= 0);
        let pn = pn as u64;
        prop_assert_eq!(decode_pn(Some(largest), pn as u32), pn);
    }
}

// ---------------------------------------------------------------- 重放窗口

#[derive(Debug, Clone)]
enum Op {
    /// 收到 pn；aead_ok = false 表示 AEAD 失败，不提交
    Recv { pn: u64, aead_ok: bool },
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    // 大部分在一个 3 倍窗口宽的范围里乱序，偶尔跳很远
    let pn = prop_oneof![9 => 0u64..12_000, 1 => 0u64..1_000_000];
    proptest::collection::vec((pn, proptest::bool::weighted(0.8)).prop_map(|(pn, aead_ok)| Op::Recv { pn, aead_ok }), 1..400)
}

proptest! {
    #[test]
    fn replay_matches_hashset_model(ops in ops()) {
        let mut w = ReplayWindow::new();
        let mut seen = HashSet::new();
        let mut largest: Option<u64> = None;
        for Op::Recv { pn, aead_ok } in ops {
            let model_ok = !seen.contains(&pn) && largest.is_none_or(|l| pn > l || l - pn < REPLAY_WINDOW);
            prop_assert_eq!(w.check(pn), model_ok, "pn {} largest {:?}", pn, largest);
            if model_ok && aead_ok {
                w.commit(pn);
                seen.insert(pn);
                largest = Some(largest.map_or(pn, |l| l.max(pn)));
            }
            prop_assert_eq!(w.largest(), largest);
        }
    }
}

#[test]
fn replay_edges() {
    let mut w = ReplayWindow::new();
    w.commit(REPLAY_WINDOW - 1);
    assert!(w.check(0), "正好在窗口左端");
    w.commit(0);
    assert_eq!(w.check_reason(0), Err(DropReason::Replay));
    w.commit(REPLAY_WINDOW);
    assert_eq!(w.check_reason(0), Err(DropReason::TooOld), "左端滑出");
    assert!(w.check(1));
    // 一次跳很远：窗口清空
    w.commit(1_000_000);
    assert!(w.check(1_000_000 - 1));
    assert_eq!(w.check_reason(REPLAY_WINDOW), Err(DropReason::TooOld));
}
