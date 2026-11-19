//! 握手：任意字节都不能 panic。
//! - 两种载荷 TLV 直接解；
//! - 整包 HS_INIT 进网关（固定密钥）：先按网关公钥重算 mac1 再交给 accept_hs_init，
//!   这样输入能越过 mac1、走到 Noise 和 TLV 那一层；
//! - 同一份字节当 HS_RESP 交给一次握手尝试（固定密钥）。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::crypto::{accept_hs_init, mac1, mac1_key, HsPeer, InitiatorAttempt, Secret32, StaticKeypair, Tai64N};
use ywp::wire::{Cid, HsInitPayload, HsRespPayload};

fuzz_target!(|data: &[u8]| {
    let _ = HsInitPayload::decode(data);
    let _ = HsRespPayload::decode(data);

    let gw = StaticKeypair::from_secret([1u8; 32]);
    let cl = StaticKeypair::from_secret([2u8; 32]);
    let psk = Secret32::new([3u8; 32]);
    let gw_mac1 = mac1_key(&gw.public());

    let mut pkt = data.to_vec();
    if pkt.len() >= 32 {
        let at = pkt.len() - 32;
        let m = mac1(&gw_mac1, &pkt[..at]);
        pkt[at..at + 16].copy_from_slice(&m);
    }
    let _ = accept_hs_init(&gw, &gw_mac1, &pkt, |_| Some(HsPeer { psk: psk.clone(), last_ts: None }));

    let init = HsInitPayload {
        ts: Tai64N::from_unix(1, 0),
        ver: 1,
        min_ver: 1,
        caps: 1,
        resume: None,
        last_fid: None,
        mode: None,
        name: None,
        bitrate_cap_kbps: None,
    };
    let mut buf = [0u8; 1232];
    if let Ok((mut att, _)) = InitiatorAttempt::start(&cl, &gw.public(), &psk, Cid(1), 0, &init, &mut buf) {
        let mut resp = data.to_vec();
        if resp.len() >= 32 {
            let at = resp.len() - 32;
            let m = mac1(&mac1_key(&cl.public()), &resp[..at]);
            resp[at..at + 16].copy_from_slice(&m);
        }
        let _ = att.read_resp(&resp);
    }
});
