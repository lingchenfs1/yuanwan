#!/bin/bash
# 定时跑一次 E8 链路长测（网关机 → 本机，下行，按帧突发）。
# 用法：tools/e8-scheduled.sh 开始时刻(HH:MM，今天) 时长秒 码率 输出目录
# 例：tools/e8-scheduled.sh 20:30 1800 15M /path/to/out
# 服务端在一条保持连接的 SSH 里运行（脱离 SSH 会被 macOS 切断局域网 UDP），测完按 pid 文件结束。
set -u
START="$1"; DUR="$2"; RATE="$3"; OUT="$4"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "${ROOT}/tools/env.sh"
mkdir -p "${OUT}"
gwssh() { "${YW_SSH[@]}" "${YW_GW_SSH}" "$@"; }

target=$(date -j -f "%Y-%m-%d %H:%M" "$(date +%Y-%m-%d) ${START}" +%s)
now=$(date +%s)
if [ "${target}" -gt "${now}" ]; then
	sleep $(( target - now ))
fi
tag="$(date +%Y%m%d-%H%M)"
gwssh 'cd ~/yw-m0 && echo $$ > probe-sched.pid && exec ./yw-probe server --port 43210 --bind-if en0' > "${OUT}/e8-${tag}-server.log" 2>&1 &
sleep 3
{
	date '+开始 %H:%M:%S'
	"${ROOT}/build/yw-probe" client "${YW_GW_IP}" --port 43210 --bind-if "${YW_BIND_IF}" --rate "${RATE}" --size 1200 \
		--duration "${DUR}" --pattern frame --trace "${OUT}/e8-${tag}-trace.csv" --timeline "${OUT}/e8-${tag}-timeline.csv" 2>&1 | grep -v -E "^…"
	date '+结束 %H:%M:%S'
} > "${OUT}/e8-${tag}.txt"
gwssh 'kill $(cat ~/yw-m0/probe-sched.pid) 2>/dev/null; rm -f ~/yw-m0/probe-sched.pid'
cat "${OUT}/e8-${tag}.txt" "${OUT}/e8-${tag}-server.log"
