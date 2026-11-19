#!/bin/bash
# M1 网关原型真机测试。在网关机上运行：ssh ... 'bash -s' < tools/m1-gw-test-remote.sh
# 网关进程放在本 SSH 会话里（脱离 SSH 会被 macOS 本地网络隐私拦截）。
# 程序用 ~/yw-m0/yw-gateway（测试前从开发机传成 yw-gateway.new 再 mv 替换）。
set -u
HOST="${YW_PS5:-$( { sed -n 's/^ *host *= *//p' ~/yw-gw/gateway.conf 2>/dev/null || true; } | head -1)}"  # PS5 地址：和网关一样取 ~/yw-gw/gateway.conf，可用 YW_PS5 覆盖
HOST="${HOST:?网关机上没有 ~/yw-gw/gateway.conf（host=PS5 地址），也没设 YW_PS5}"
DIR="${HOME}/yw-gw-test"
cd ~/yw-m0 || exit 1
mkdir -p "${DIR}"
ctl() { echo "$*" | nc -U "${DIR}/control.sock"; }
now() { perl -MTime::HiRes=time -e 'printf "%.1f", time'; }
wait_state() { # wait_state 状态 超时秒
	local want="$1" limit="$2" t0; t0=$(now)
	for i in $(seq 1 $((limit * 2))); do
		if ctl status 2>/dev/null | grep -q "\"state\":\"${want}\""; then
			echo "  → ${want}（$(perl -e "printf '%.1f', $(now) - ${t0}") s）"
			return 0
		fi
		sleep 0.5
	done
	echo "  → ${limit} s 内没有到「${want}」：$(ctl status)"
	return 1
}
start_gw() {
	./yw-gateway --host "${HOST}" --dir "${DIR}" --stats 10 >> "${DIR}/stderr.log" 2>&1 &
	GW=$!
	sleep 1
}
stop_gw() {
	ctl quit
	for i in $(seq 1 40); do kill -0 "${GW}" 2>/dev/null || break; sleep 0.5; done
	kill -0 "${GW}" 2>/dev/null && { echo "  进程 20 s 没退出，发 SIGTERM"; kill "${GW}"; }
	wait "${GW}" 2>/dev/null
	echo "  退出码 $?"
}
timed() { # timed 命令…：执行控制命令并打印回复和耗时
	local t0 r; t0=$(now)
	r=$(ctl "$@")
	echo "  $* → ${r}（$(perl -e "printf '%.2f', $(now) - ${t0}") s）"
}

# 上一轮的输出移到 old/<时间>/，不直接删
OLD="${DIR}/old/$(date +%Y%m%d-%H%M%S)"
mkdir -p "${OLD}"
for f in "${DIR}/stderr.log" "${DIR}/gateway.jsonl" "${DIR}/paused" "${DIR}"/m1-*; do
	[ -e "${f}" ] && mv "${f}" "${OLD}/"
done
echo "== 1. 启动"
start_gw
wait_state 串流中 60
sleep 3
ctl status

echo "== 2. 串流中的命令"
timed idr
timed resume
timed record m1-rec1.h265 10
sleep 1.5
timed record m1-rec2.h265 2
timed snap m1-snap.h265
for i in $(seq 1 20); do [ -f "${DIR}/m1-snap.h265" ] && break; sleep 0.5; done
sleep 1
ls -la "${DIR}"/m1-* 2>/dev/null | awk '{print "  " $5, $9}'
echo "  （期望：m1-rec1.tmp 已删除；rec2 被 snap 取消也删除；只有 m1-snap.h265）"

echo "== 3. restart 后马上 pause（建会话中途停止要快）"
timed restart
sleep 1.5
ctl status | grep -o '"state":"[^"]*"' | sed 's/^/  /'
timed pause
timed idr
ls "${DIR}/paused" >/dev/null 2>&1 && echo "  暂停标记：$(cat "${DIR}/paused")" || echo "  没有暂停标记！"

echo "== 4. 暂停中退出，再启动应仍暂停"
stop_gw
start_gw
sleep 2
ctl status | grep -o '"state":"[^"]*","pause_reason":"[^"]*"' | sed 's/^/  /'
timed resume
wait_state 串流中 60

echo "== 5. sleep（让 PS5 待机）"
timed sleep
sleep 15
./yw-m0-probe status "${HOST}" | head -1

echo "== 6. resume（网关自动唤醒并重连）"
timed resume
wait_state 串流中 90
ctl status

echo "== 7. quit（正常退出）"
stop_gw
ls "${DIR}/paused" >/dev/null 2>&1 && echo "  还有暂停标记！" || echo "  没有暂停标记（正确）"
ls "${DIR}"/*.tmp 2>/dev/null && echo "  有残留临时文件！" || echo "  没有残留临时文件（正确）"
echo "== 网关日志（stderr，警告和关键事件）"
grep -E " [EW] |\[(record|control|gateway|wake)\]|会话结束|发起会话|串流中（" "${DIR}/stderr.log" | cut -c1-170 | tail -60
