#!/usr/bin/env python3
"""独立核对 golden.txt：只用 Python 标准库，按规格 ywp.md v0.2 从零重建每个包，逐字节比对。

X25519（RFC 7748）、ChaCha20-Poly1305（RFC 8439）、Noise_IKpsk2 的对称状态（Noise 规范 rev 34）都在本文件里实现，
不依赖 Rust 那边的任何代码，所以能发现「两边一起错」以外的所有线上格式问题：
prologue、psk 位置、HS_INIT 补齐和 mac1 位置、Split 的方向、iv 的 HKDF 派生、nonce 布局、AD、各块的字节布局。

用法：python3 -I verify_golden.py [golden.txt 的路径]
"""
import hashlib
import hmac
import struct
import sys
from pathlib import Path

# ---------------------------------------------------------------- X25519

P = 2**255 - 19


def x25519(k: bytes, u: bytes) -> bytes:
    k = bytearray(k)
    k[0] &= 248
    k[31] &= 127
    k[31] |= 64
    k = int.from_bytes(k, "little")
    u = int.from_bytes(u, "little") & ((1 << 255) - 1)
    x1, x2, z2, x3, z3, swap = u, 1, 0, u, 1, 0
    for t in reversed(range(255)):
        kt = (k >> t) & 1
        swap ^= kt
        if swap:
            x2, x3, z2, z3 = x3, x2, z3, z2
        swap = kt
        a, b = x2 + z2, x2 - z2
        aa, bb = a * a % P, b * b % P
        e = aa - bb
        c, d = x3 + z3, x3 - z3
        da, cb = d * a % P, c * b % P
        x3 = (da + cb) ** 2 % P
        z3 = x1 * (da - cb) ** 2 % P
        x2 = aa * bb % P
        z2 = e * (aa + 121665 * e) % P
    if swap:
        x2, z2 = x3, z3
    return (x2 * pow(z2, P - 2, P) % P).to_bytes(32, "little")


def x25519_pub(k: bytes) -> bytes:
    return x25519(k, (9).to_bytes(32, "little"))


# ---------------------------------------------------------------- ChaCha20-Poly1305


def _rotl(v, c):
    return ((v << c) & 0xFFFFFFFF) | (v >> (32 - c))


def _block(key: bytes, counter: int, nonce: bytes) -> bytes:
    st = [0x61707865, 0x3320646E, 0x79622D32, 0x6B206574, *struct.unpack("<8I", key), counter, *struct.unpack("<3I", nonce)]
    w = st[:]

    def qr(a, b, c, d):
        w[a] = (w[a] + w[b]) & 0xFFFFFFFF; w[d] = _rotl(w[d] ^ w[a], 16)
        w[c] = (w[c] + w[d]) & 0xFFFFFFFF; w[b] = _rotl(w[b] ^ w[c], 12)
        w[a] = (w[a] + w[b]) & 0xFFFFFFFF; w[d] = _rotl(w[d] ^ w[a], 8)
        w[c] = (w[c] + w[d]) & 0xFFFFFFFF; w[b] = _rotl(w[b] ^ w[c], 7)

    for _ in range(10):
        qr(0, 4, 8, 12); qr(1, 5, 9, 13); qr(2, 6, 10, 14); qr(3, 7, 11, 15)
        qr(0, 5, 10, 15); qr(1, 6, 11, 12); qr(2, 7, 8, 13); qr(3, 4, 9, 14)
    return struct.pack("<16I", *[(w[i] + st[i]) & 0xFFFFFFFF for i in range(16)])


def _chacha20(key, counter, nonce, data):
    out = bytearray()
    for i in range(0, len(data), 64):
        ks = _block(key, counter + i // 64, nonce)
        out += bytes(a ^ b for a, b in zip(data[i:i + 64], ks))
    return bytes(out)


def _poly1305(key, msg):
    r = int.from_bytes(key[:16], "little") & 0x0FFFFFFC0FFFFFFC0FFFFFFC0FFFFFFF
    s = int.from_bytes(key[16:], "little")
    acc = 0
    for i in range(0, len(msg), 16):
        acc = (acc + int.from_bytes(msg[i:i + 16] + b"\x01", "little")) * r % (2**130 - 5)
    return ((acc + s) % 2**128).to_bytes(16, "little")


def _pad16(b):
    return b"\x00" * (-len(b) % 16)


def aead_seal(key, nonce, ad, pt):
    otk = _block(key, 0, nonce)[:32]
    ct = _chacha20(key, 1, nonce, pt)
    tag = _poly1305(otk, ad + _pad16(ad) + ct + _pad16(ct) + struct.pack("<QQ", len(ad), len(ct)))
    return ct + tag


# ---------------------------------------------------------------- Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s


def blake2s(*parts, **kw):
    h = hashlib.blake2s(**kw)
    for p in parts:
        h.update(p)
    return h.digest()


def hkdf_noise(ck, ikm, n):
    tk = hmac.new(ck, ikm, hashlib.blake2s).digest()
    outs, prev = [], b""
    for i in range(1, n + 1):
        prev = hmac.new(tk, prev + bytes([i]), hashlib.blake2s).digest()
        outs.append(prev)
    return outs


class Sym:
    def __init__(self, name: bytes):
        self.h = name if len(name) == 32 else blake2s(name)
        self.ck = self.h
        self.k = None
        self.n = 0

    def mix_hash(self, d):
        self.h = blake2s(self.h, d)

    def mix_key(self, ikm):
        self.ck, self.k = hkdf_noise(self.ck, ikm, 2)
        self.n = 0

    def mix_key_and_hash(self, ikm):
        self.ck, th, self.k = hkdf_noise(self.ck, ikm, 3)
        self.mix_hash(th)
        self.n = 0

    def encrypt_and_hash(self, pt):
        if self.k is None:
            self.mix_hash(pt)
            return pt
        c = aead_seal(self.k, b"\x00" * 4 + struct.pack("<Q", self.n), self.h, pt)
        self.n += 1
        self.mix_hash(c)
        return c

    def split(self):
        return hkdf_noise(self.ck, b"", 2)


# ---------------------------------------------------------------- 线上格式（规格 §4.4、§4.5、§5）


def tlv(t, v):
    return bytes([t]) + struct.pack(">H", len(v)) + v


def hkdf_iv(h):
    prk = hmac.new(b"ywp v0.1 iv", h, hashlib.blake2s).digest()
    return [hmac.new(prk, info + b"\x01", hashlib.blake2s).digest()[:12] for info in (b"c2s", b"s2c")]


def data_packet(flags, cid, path_id, pn, key, iv, plain):
    hdr = bytes([flags]) + cid + bytes([path_id]) + struct.pack(">I", pn & 0xFFFFFFFF)
    nonce = bytes(a ^ b for a, b in zip(b"\x00\x00\x00" + bytes([path_id]) + struct.pack(">Q", pn), iv))
    return hdr + aead_seal(key, nonce, hdr, plain)


def main():
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).with_name("golden.txt")
    g = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            k, v = line.split(" = ")
            g[k] = bytes.fromhex(v)

    assert x25519_pub(bytes.fromhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a")) == bytes.fromhex(
        "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
    ), "X25519 自检（RFC 7748 §6.1）"

    s_c, s_g = g["client_static_secret"], g["gw_static_secret"]
    S_c, S_g = x25519_pub(s_c), x25519_pub(s_g)
    assert S_c == g["client_static_public"] and S_g == g["gw_static_public"]
    e_c, e_g = g["client_ephemeral_secret"], g["gw_ephemeral_secret"]
    E_c, E_g = x25519_pub(e_c), x25519_pub(e_g)
    psk, cid, ts = g["psk"], g["cid"], g["ts"]

    # ---- HS_INIT ----
    payload = (
        tlv(0x01, ts)
        + tlv(0x02, bytes([1, 1]))
        + tlv(0x03, struct.pack(">I", 0b11))
        + tlv(0x04, b"\x11" * 8 + struct.pack(">I", 0x00010002))
        + tlv(0x05, struct.pack(">HB", 0xFFFF, 3))
        + tlv(0x06, bytes([0, 1]))
        + tlv(0x07, "客户端-1".encode())
        + tlv(0x08, struct.pack(">I", 15000))
    )
    payload += b"\x00" * (1090 - len(payload))
    hdr = bytes([0x48]) + cid + b"\x00" + struct.pack(">I", 0)
    ss = Sym(b"Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s")
    ss.mix_hash(b"YWP v0.1" + cid)  # prologue
    ss.mix_hash(S_g)  # <- s
    ss.mix_hash(E_c); ss.mix_key(E_c)  # e（psk 模式下还要 MixKey）
    ss.mix_key(x25519(e_c, S_g))  # es
    enc_s = ss.encrypt_and_hash(S_c)  # s
    ss.mix_key(x25519(s_c, S_g))  # ss
    enc_p = ss.encrypt_and_hash(payload)
    body = hdr + E_c + enc_s + enc_p
    assert len(body) == 1200
    init = body + blake2s(body, digest_size=16, key=blake2s(b"mac1----", S_g)) + b"\x00" * 16
    assert init == g["hs_init"], "hs_init 不符"

    # ---- HS_RESP ----
    token = blake2s(cid, digest_size=16, key=g["reset_key"])
    rpay = (
        tlv(0x02, bytes([1, 1]))
        + tlv(0x03, struct.pack(">I", 0b11))
        + tlv(0x10, b"\x00")
        + tlv(0x11, token)
        + tlv(0x12, bytes([4]) + b"\x22" * 8 + struct.pack(">I", 0xABCD0001) + b"\x01")
    )
    rhdr = bytes([0x50]) + cid + b"\x00" + struct.pack(">I", 0)
    ss.mix_hash(E_g); ss.mix_key(E_g)  # e
    ss.mix_key(x25519(e_c, E_g))  # ee
    ss.mix_key(x25519(s_c, E_g))  # se
    ss.mix_key_and_hash(psk)  # psk（位置 2：第二条消息末尾）
    rbody = rhdr + E_g + ss.encrypt_and_hash(rpay)
    resp = rbody + blake2s(rbody, digest_size=16, key=blake2s(b"mac1----", S_c)) + b"\x00" * 16
    assert resp == g["hs_resp"], "hs_resp 不符"

    # ---- 方向密钥和 iv ----
    k_c2s, k_s2c = ss.split()
    iv_c2s, iv_s2c = hkdf_iv(ss.h)
    assert (k_c2s, iv_c2s, k_s2c, iv_s2c) == (g["k_c2s"], g["iv_c2s"], g["k_s2c"], g["iv_s2c"]), "方向密钥或 iv 不符"

    # ---- DATA c2s：SendTS | PING(1) | INPUT（1 个事件）----
    state = struct.pack(">IBB4h", 0, 0, 0, 0, 0, 0, 0) + b"\xff\x00\x00\x00" * 2 + b"\x00" * 12 + struct.pack(">IH", 0, 0)
    assert len(state) == 40
    inp = struct.pack(">HB", 1, 0x80 | 1) + state + struct.pack(">HBBH", 1, 0, 1, 2)
    plain = struct.pack(">I", 0x00010000) + tlv(0x20, struct.pack(">H", 1)) + tlv(0x38, inp)
    assert data_packet(0x43, cid, 0, 0, k_c2s, iv_c2s, plain) == g["data_c2s_pn0"], "data_c2s 不符"

    # ---- DATA s2c：SendTS | PONG | VIDEO 源符号（K、L 位；cfg = 1），PN = 2³² + 2 ----
    nal = bytes.fromhex("000000012601")
    video = bytes([0x10 | 0x08 | 0x02]) + struct.pack(">HIHB", 0, 0, 40, 1 << 1) + struct.pack(">H", len(nal)) + nal
    plain = struct.pack(">I", 0x00010100) + tlv(0x21, struct.pack(">HIH", 1, 0x00010000, 5)) + video
    assert data_packet(0x41, cid, 0, (1 << 32) | 2, k_s2c, iv_s2c, plain) == g["data_s2c_pn100000002"], "data_s2c 不符"

    # ---- RESET ----
    reset = bytes([0x70]) + cid + b"\x00" + bytes.fromhex("a1b2c3d4") + b"\xee" * 8 + token
    assert reset == g["reset"], "reset 不符"
    print("golden.txt 全部一致")


if __name__ == "__main__":
    main()
