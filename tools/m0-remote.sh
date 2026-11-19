#!/bin/bash
# 远程操作 PS5（经网关机上的常驻会话）。
#   tools/m0-remote.sh start [额外的 stream 参数...]   在网关机后台启动会话（带控制管道）
#   tools/m0-remote.sh cmd "press cross"               发一条命令（见 main.c 里控制管道的说明）
#   tools/m0-remote.sh snap [名字]                     截屏：解码成 build/snaps/名字.png
#   tools/m0-remote.sh log [行数]                      看会话日志的最后几行（不含逐帧统计）
#   tools/m0-remote.sh stop                            结束会话
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "${ROOT}/tools/env.sh"   # 网关机登录、PS5 地址都在 tools/local.env（不进仓库）
HOST="${YW_PS5:?在 tools/local.env 里设置 YW_PS5（PS5 地址）}"
SSH=("${YW_SSH[@]}" -o ConnectTimeout=10 "${YW_GW_SSH}")
SCP=("${YW_SCP[@]}")

action="${1:-}"
shift || true
case "${action}" in
	start)
		# 注意：必须让这条 SSH 一直连着（调用方把本命令放到后台）。实测 nohup 脱离 SSH 会话后，
		# macOS 会在 SSH 退出时切断该进程的局域网 UDP，PS5 随即关闭串流端口。
		extra="$*"
		exec "${SSH[@]}" "cd ~/yw-m0 && rm -f ctl && echo \$\$ > live.pid && exec ./yw-m0-probe stream ${HOST} --ctl ~/yw-m0/ctl --duration 3600 --log ~/yw-m0/live.log ${extra} > live.out 2>&1 < /dev/null"
		;;
	cmd)
		"${SSH[@]}" "~/yw-m0/ctl-send $*"
		;;
	snap)
		name="${1:-snap}"
		mkdir -p "${ROOT}/build/snaps"
		"${SSH[@]}" "rm -f ~/yw-m0/${name}.h265; ~/yw-m0/ctl-send snap \$HOME/yw-m0/${name}.h265 >/dev/null || exit 1; for i in \$(seq 1 50); do test -f ~/yw-m0/${name}.h265 && break; sleep 0.2; done; test -f ~/yw-m0/${name}.h265"
		"${SCP[@]}" "${YW_GW_SSH}:yw-m0/${name}.h265" "${ROOT}/build/snaps/${name}.h265"
		ffmpeg -loglevel error -y -f hevc -i "${ROOT}/build/snaps/${name}.h265" -frames:v 1 -vf scale=960:-1 "${ROOT}/build/snaps/${name}.png"
		echo "${ROOT}/build/snaps/${name}.png"
		;;
	log)
		"${SSH[@]}" "grep -v -E 'yw-frame|yw-sample|Sending Congestion|Frame Processor|FEC successful' ~/yw-m0/live.log | tail -${1:-20}"
		;;
	stop)
		"${SSH[@]}" "~/yw-m0/ctl-send quit; sleep 3; p=\$(cat ~/yw-m0/live.pid 2>/dev/null || true); if [ -n \"\$p\" ] && kill -0 \$p 2>/dev/null; then kill \$p; echo 已发 SIGTERM; fi; grep -E 'yw-run\\] 汇总|会话结束' ~/yw-m0/live.log | tail -2"
		;;
	*)
		sed -n '2,8p' "$0"
		exit 1
		;;
esac
