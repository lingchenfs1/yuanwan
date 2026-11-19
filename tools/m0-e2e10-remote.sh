#!/bin/bash
# E2（IDR 响应）和 E10（码率上限）。在网关机上运行：ssh ... 'bash -s' < tools/m0-e2e10-remote.sh
# 会话进程和发命令的子进程都在同一个 SSH 会话里（脱离 SSH 后 macOS 会切断局域网 UDP）。
set -u
HOST="${YW_PS5:-$( { sed -n 's/^ *host *= *//p' ~/yw-gw/gateway.conf 2>/dev/null || true; } | head -1)}"  # PS5 地址：和网关一样取 ~/yw-gw/gateway.conf，可用 YW_PS5 覆盖
HOST="${HOST:?网关机上没有 ~/yw-gw/gateway.conf（host=PS5 地址），也没设 YW_PS5}"
cd ~/yw-m0 || exit 1
send() { ./ctl-send "$@" > /dev/null 2>&1; }

mover_start() {
	rm -f mover.stop
	(
		while [ ! -f mover.stop ]; do
			send rstick 26000 0 900; sleep 1
			send rstick -26000 0 900; sleep 1
		done
	) &
	MOVER=$!
}
mover_stop() { touch mover.stop; wait "${MOVER}" 2>/dev/null; rm -f mover.stop; }

# 启动会话（失败就按冷却时间重试），等到连上
start_session() { # start_session 日志名 时长 [额外参数...]
	local name="$1" dur="$2"; shift 2
	for attempt in $(seq 1 10); do
		rm -f ctl
		./yw-m0-probe stream "${HOST}" --ctl ~/yw-m0/ctl --duration "${dur}" --log "${name}.log" "$@" > /dev/null 2>&1 &
		SESSION=$!
		for i in $(seq 1 30); do
			grep -q "首帧" "${name}.log" 2>/dev/null && return 0
			kill -0 "${SESSION}" 2>/dev/null || break
			sleep 0.5
		done
		wait "${SESSION}" 2>/dev/null
		echo "${name}：第 ${attempt} 次没连上（$(grep -o '会话结束：.*' "${name}.log" | head -1)），5 秒后重试"
		sleep 5
	done
	return 1
}

# ---- E2：15 Mbps，第 10~50 秒每 10 秒请求一次 IDR ----
if start_session e2 70; then
	send inject 0
	mover_start
	for t in 10 20 30 40 50; do
		sleep 10
		send idr
	done
	mover_stop
	wait "${SESSION}"
	echo "E2 完成"
fi
sleep 8

# ---- E10：申请 30 / 50 / 100 Mbps，各 60 秒 ----
for br in 30000 50000 100000; do
	if start_session "e10-${br}" 60 --bitrate "${br}"; then
		send inject 0
		mover_start
		wait "${SESSION}"
		mover_stop
		echo "E10 ${br} kbps 完成"
	fi
	sleep 8
done
