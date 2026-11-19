#!/usr/bin/env python3
"""汇总 yw-m0-probe stream 的日志：按「注入丢包」阶段切分，输出 Markdown 表格。

用法：python3 tools/m0-analyze.py 日志文件 [更多日志文件...]
"""
import re
import statistics
import sys

LINE = re.compile(r"^(\d+\.\d+) ([EWIVD]) (.*)$")
FRAME = re.compile(r"\[yw-frame\] idx=(-?\d+) prof=(-?\d+) src=(\d+)/(\d+) fec=(\d+)/(\d+) fec_raw=(\d+) t0=\d+ span_us=(\d+)(?: unit=(\d+))?")
SAMPLE = re.compile(r"\[yw-sample\] n=(\d+) size=(\d+) key=(\d) lost=(-?\d+) recovered=(\d)")
QUALITY = re.compile(r"connection quality: target_bitrate=(-?\d+), upstream_bitrate=(-?\d+), upstream_loss=([\d.]+), disable_upstream_audio=\d+, rtt=([\d.]+), loss=(-?\d+)")
MEASURED = re.compile(r"measured bitrate: ([\d.]+) MBit/s")
SEC = re.compile(r"\[yw-sec\] t=(\d+) fps=([\d.]+) mbps=([\d.]+)")
PLAN_LOSS = re.compile(r"\[yw-plan\] t=([\d.]+) 注入丢包上报 (关闭 )?(-?[\d.]+)")
PLAN_IDR = re.compile(r"\[yw-plan\] t=([\d.]+) 请求 IDR")
PROFILE = re.compile(r"Switched to profile (\d+), resolution: (\d+)x(\d+)")
FIRST = re.compile(r"\[yw-run\] 首帧：发起会话后 ([\d.]+) s")


def pct(values, p):
    if not values:
        return float("nan")
    values = sorted(values)
    k = min(len(values) - 1, max(0, int(round(p / 100 * (len(values) - 1)))))
    return values[k]


def mean(values):
    return statistics.fmean(values) if values else float("nan")


class Phase:
    def __init__(self, start, label):
        self.start = start
        self.label = label
        self.frames = []      # (src_rx, src_exp, fec_rx, fec_exp, fec_raw, span_us)
        self.samples = []     # (size, key, lost, recovered)
        self.targets = []     # target_bitrate
        self.measured = []    # MBit/s
        self.sec_mbps = []
        self.sec_fps = []
        self.profiles = []
        self.units = []       # 每帧的单元大小（字节）


def analyze(path):
    phases = [Phase(0.0, "开始")]
    idr_requests = []
    key_times = []
    first_frame = None
    profile_events = []
    with open(path, encoding="utf-8", errors="replace") as f:
        for raw in f:
            m = LINE.match(raw.rstrip("\n"))
            if not m:
                continue
            t = float(m.group(1))
            msg = m.group(3)
            ph = phases[-1]
            if (mm := PLAN_LOSS.search(msg)):
                value = float(mm.group(3))
                label = "正常上报" if value < 0 else f"注入 {value * 100:.1f}%"
                phases.append(Phase(t, label))
            elif (mm := FRAME.search(msg)):
                g = mm.groups()
                ph.frames.append(tuple(int(x) for x in g[2:8]))
                if g[8]:
                    ph.units.append(int(g[8]))
            elif (mm := SAMPLE.search(msg)):
                size, key, lost, rec = int(mm.group(2)), int(mm.group(3)), int(mm.group(4)), int(mm.group(5))
                ph.samples.append((size, key, lost, rec))
                if key:
                    key_times.append((t, size))
            elif (mm := QUALITY.search(msg)):
                ph.targets.append(int(mm.group(1)))
            elif (mm := MEASURED.search(msg)):
                ph.measured.append(float(mm.group(1)))
            elif (mm := SEC.search(msg)):
                ph.sec_fps.append(float(mm.group(2)))
                ph.sec_mbps.append(float(mm.group(3)))
            elif PLAN_IDR.search(msg):
                idr_requests.append(t)
            elif (mm := PROFILE.search(msg)):
                profile_events.append((t, int(mm.group(1)), f"{mm.group(2)}x{mm.group(3)}"))
            elif (mm := FIRST.search(msg)):
                first_frame = float(mm.group(1))

    print(f"## {path}\n")
    if first_frame is not None:
        print(f"- 发起会话到首帧：{first_frame:.3f} s")
    if profile_events:
        print("- 分辨率档位切换：" + "；".join(f"{t:.1f}s → 档位 {p}（{r}）" for t, p, r in profile_events))
    print()
    print("| 阶段 | 起点(s) | 帧数 | PS5 目标码率(Mbps) 均值/最小/最大 | 实测(Mbps) | 平均源单元 | FEC 冗余 均值(原始) | 缺单元帧比例 | 首末包跨度 P50/P95(ms) | 关键帧 | 平均帧大小(KB) | 单元大小 P50/最大(B) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for ph in phases:
        if not ph.frames and not ph.samples and not ph.targets:
            continue
        src_exp = [fr[1] for fr in ph.frames]
        fec_ratio = [fr[4] / fr[1] for fr in ph.frames if fr[1] > 0]
        incomplete = sum(1 for fr in ph.frames if fr[0] < fr[1])
        spans = [fr[5] / 1000 for fr in ph.frames]
        targets = [x / 1e6 for x in ph.targets if x > 0]
        keys = sum(1 for s in ph.samples if s[1])
        sizes = [s[0] / 1024 for s in ph.samples]
        tgt = f"{mean(targets):.1f} / {min(targets):.1f} / {max(targets):.1f}" if targets else "—"
        meas = f"{mean(ph.sec_mbps):.1f}" if ph.sec_mbps else (f"{mean(ph.measured):.1f}" if ph.measured else "—")
        inc = f"{incomplete / len(ph.frames) * 100:.2f}%" if ph.frames else "—"
        print(f"| {ph.label} | {ph.start:.1f} | {len(ph.frames)} | {tgt} | {meas} | {mean(src_exp):.1f} | "
              f"{mean(fec_ratio) * 100:.1f}% | {inc} | {pct(spans, 50):.2f} / {pct(spans, 95):.2f} | {keys} | {mean(sizes):.1f} | "
              f"{(str(pct(ph.units, 50)) + ' / ' + str(max(ph.units))) if ph.units else '—'} |")
    if idr_requests:
        print("\n| IDR 请求时刻(s) | 关键帧到达延迟(ms) | 关键帧大小(KB) |")
        print("|---|---|---|")
        for rt in idr_requests:
            nxt = next(((kt, ks) for kt, ks in key_times if kt >= rt), None)
            if nxt:
                print(f"| {rt:.2f} | {(nxt[0] - rt) * 1000:.0f} | {nxt[1] / 1024:.1f} |")
            else:
                print(f"| {rt:.2f} | 未收到 | — |")
    print()


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    for path in sys.argv[1:]:
        analyze(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
