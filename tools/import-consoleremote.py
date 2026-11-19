#!/usr/bin/env python3
"""把 Console Remote 导出的 PlayStationSettings_*.json 转成 yw-m0-probe 的凭据格式，输出到标准输出。

只取会话需要的 rpRegistKey、rpKey、rpKeyType；PSN 的 refreshToken、账号 ID、在线 ID 一律不读不写。
不要把输出重定向到仓库里，直接经 SSH 写到网关机：

  python3 -I tools/import-consoleremote.py PlayStationSettings_XXXX.json [主机键名] | \\
    ssh ... <用户>@<网关机> 'umask 077; mkdir -p ~/.config/yw && cat > ~/.config/yw/ps5.cred'

导出文件里通常每台主机一项。只有一台时自动选中；有多台时用第二个参数指定主机键名。
"""
import base64
import json
import re
import sys


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 1
    with open(sys.argv[1], encoding="utf-8") as f:
        d = json.load(f)
    if "rpRegistKey" not in d:
        # 顶层按主机分项：找含 rpRegistKey 的那一项（不依赖具体的主机名）
        hosts = {k: v for k, v in d.items() if isinstance(v, dict) and "rpRegistKey" in v}
        if len(sys.argv) == 3:
            if sys.argv[2] not in hosts:
                print("导出文件里没有这个主机键名", file=sys.stderr)
                return 1
            d = hosts[sys.argv[2]]
        elif len(hosts) == 1:
            d = next(iter(hosts.values()))
        else:
            print(f"导出文件里找到 {len(hosts)} 台主机，请用第二个参数指定主机键名", file=sys.stderr)
            return 1
    rk = base64.b64decode(d["rpRegistKey"])
    mk = base64.b64decode(d["rpKey"])
    head = rk.rstrip(b"\0")
    if len(rk) != 16 or not re.fullmatch(rb"[0-9a-fA-F]{1,16}", head) or any(rk[len(head):]):
        print("rpRegistKey 格式不对（应为 16 字节：ASCII 十六进制加零填充）", file=sys.stderr)
        return 1
    if len(mk) != 16 or not any(mk):
        print("rpKey 格式不对（应为 16 字节、非全零）", file=sys.stderr)
        return 1
    if str(d.get("target")) != "1000100":
        print(f"注意：target={d.get('target')}，不是 PS5（1000100）", file=sys.stderr)
    nick = "".join(ch for ch in str(d.get("hostName", "")) if ch.isprintable() and ch not in "=\n")[:32]
    print("# yw PS5 凭据（由 Console Remote 导出配置转换，勿外传、勿提交）")
    print(f"target={int(d.get('target', 1000100))}")
    print("server_mac=000000000000")
    print(f"nickname={nick}")
    print(f"rp_key_type={int(d.get('rpKeyType', 0))}")
    print("console_pin=0")
    print(f"regist_key={rk.hex()}")
    print(f"rp_key={mk.hex()}")
    print(f"主机 {d.get('hostAddr', '?')}，转换完成", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
