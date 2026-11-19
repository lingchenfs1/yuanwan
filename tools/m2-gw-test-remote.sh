#!/bin/bash
# M2a 网关真机测试（草稿，还没在真机上跑过）。在网关机上运行：ssh ... 'bash -s' < tools/m2-gw-test-remote.sh
# 网关进程放在本 SSH 会话里（脱离 SSH 会被 macOS 本地网络隐私拦截）。
# 程序用 ~/yw-m0/yw-gateway（测试前从开发机传成 yw-gateway.new 再 mv 替换）。
#
# 测的是网关这一侧（客户端在开发机上，由 tools/m2-client-*.sh 另外跑）：
#   1. 没有 keys/：不起 YWP，控制口命令和会话行为和 M1 一样（计划 §3.1 真机验收第 5 条）；
#   2. keys/ 权限不对：不起 YWP，PS5 会话照常；
#   3. 配对一个客户端后起 YWP：端口在听，status 带 YWP 摘要，clients 列出配对；
#   4. 可选：等开发机上的客户端连进来，记录接入和转发计数，再试 idr、restart；
#   5. quit 正常退出（有客户端时先发 CLOSE(5)）。
#
# 环境变量：
#   YW_CLIENT_PUB   必填：测试客户端的公钥（64 位十六进制，开发机上 `yw-client pubkey` 打印的那串）
#   YW_CLIENT_NAME  测试客户端的名字（默认 m2test）
#   YW_PS5          PS5 地址（默认取 ~/yw-gw/gateway.conf 的 host=，和网关一样）
#   YW_YWP_PORT     测试实例的 UDP 端口（默认 9421，不和正式网关的 9420 冲突）
#   YW_BIND_IF      YWP 绑哪块网卡（默认取 ~/yw-gw/gateway.conf 的 ywp_bind_if=，没有就不绑）
#   YW_WAIT_CLIENT  第 4 步等客户端连进来多少秒（默认 0：不等）
#   YW_PAUSE_PROD   正式网关（~/yw-gw/control.sock）在跑时必须设成 1：测试前 pause、测试后 resume，
#                   否则脚本直接退出（同一时刻只能有一个网关持有 PS5 会话）
#
# 配对串含 PSK：本脚本只让网关把它写进 600 权限的文件（<测试目录>/pair-<名字>.txt），不往 stdout 打印。
# 开发机导入：ssh ... 'cat ~/yw-gw-test/pair-<名字>.txt' | yw-client pair import
# 导入后这个文件要删掉——删除前先问用户。
set -u
CONF="${HOME}/yw-gw/gateway.conf"
conf_get() { { sed -n "s/^ *$1 *= *//p" "${CONF}" 2>/dev/null || true; } | head -1; }
HOST="${YW_PS5:-$(conf_get host)}"
HOST="${HOST:?网关机上没有 ~/yw-gw/gateway.conf（host=PS5 地址），也没设 YW_PS5}"
PUB="${YW_CLIENT_PUB:?要设 YW_CLIENT_PUB（测试客户端的公钥，64 位十六进制）}"
NAME="${YW_CLIENT_NAME:-m2test}"
PORT="${YW_YWP_PORT:-9421}"
BIND_IF="${YW_BIND_IF:-$(conf_get ywp_bind_if)}"
WAIT_CLIENT="${YW_WAIT_CLIENT:-0}"
DIR="${HOME}/yw-gw-test"           # 第 2~5 步的网关目录（keys/ 在这里，留着复用）
DIR_M1="${HOME}/yw-gw-test/m1"     # 第 1 步：没有 keys/ 的网关目录
PROD_SOCK="${HOME}/yw-gw/control.sock"
TAB="$(printf '\t')"
cd ~/yw-m0 || exit 1
test -x ./yw-gateway || { echo "~/yw-m0/yw-gateway 不存在"; exit 1; }
mkdir -p "${DIR}" "${DIR_M1}"
chmod 700 "${DIR}" "${DIR_M1}"

CUR="${DIR}"
ctl() { echo "$*" | nc -U "${CUR}/control.sock"; }
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
start_gw() { # start_gw 目录
	CUR="$1"
	./yw-gateway --host "${HOST}" --dir "${CUR}" --stats 10 >> "${CUR}/stderr.log" 2>&1 &
	GW=$!
	sleep 1.5
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
ywp_running() { ctl status 2>/dev/null | perl -ne 'print /"ywp":\{"running":true/ ? "true\n" : "false\n"; exit'; }
ywp_why() { ctl status 2>/dev/null | perl -ne 'print "  why: $1\n" if /"ywp":\{"running":false,"why":"([^"]*)"/'; }
udp_listening() { lsof -nP -iUDP:"${PORT}" 2>/dev/null | grep -q yw-gatewa; }
paired() { ./yw-gateway pair list --dir "${DIR}" 2>&1 | grep -q "^${NAME}${TAB}"; }

# ---- 正式网关在跑时先暂停它，退出时恢复
PAUSED_PROD=0
if [ -S "${PROD_SOCK}" ] && echo status | nc -U "${PROD_SOCK}" >/dev/null 2>&1; then
	if [ "${YW_PAUSE_PROD:-0}" != 1 ]; then
		echo "正式网关在跑（${PROD_SOCK}）。同一时刻只能有一个网关持有 PS5 会话：确认可以暂停它之后设 YW_PAUSE_PROD=1 再跑"
		exit 1
	fi
	echo "== 0. 暂停正式网关：$(echo pause | nc -U "${PROD_SOCK}")"
	PAUSED_PROD=1
	sleep 8   # 正常结束后 PS5 冷却 3~6 s
fi
restore_prod() {
	if [ "${PAUSED_PROD}" = 1 ]; then
		echo "== 恢复正式网关：$(echo resume | nc -U "${PROD_SOCK}")"
	fi
}
trap restore_prod EXIT

# 上一轮的输出移到 old/<时间>/，不直接删（keys/ 留着复用）
OLD="${DIR}/old/$(date +%Y%m%d-%H%M%S)"
mkdir -p "${OLD}/m1"
for f in "${DIR}/stderr.log" "${DIR}/gateway.jsonl" "${DIR}/paused" "${DIR}"/m2-* "${DIR}/gateway.conf"; do
	[ -e "${f}" ] && mv "${f}" "${OLD}/"
done
for f in "${DIR_M1}/stderr.log" "${DIR_M1}/gateway.jsonl" "${DIR_M1}/paused" "${DIR_M1}"/m2-* "${DIR_M1}/gateway.conf"; do
	[ -e "${f}" ] && mv "${f}" "${OLD}/m1/"
done
[ -e "${DIR_M1}/keys" ] && mv "${DIR_M1}/keys" "${OLD}/m1/keys"
for d in "${DIR}" "${DIR_M1}"; do
	{
		echo "ywp_port=${PORT}"
		[ -n "${BIND_IF}" ] && echo "ywp_bind_if=${BIND_IF}"
	} > "${d}/gateway.conf"
done
echo "测试实例：UDP ${PORT}，网卡 ${BIND_IF:-不绑}"

echo "== 1. 没有 keys/：不起 YWP，M1 的命令照常"
start_gw "${DIR_M1}"
wait_state 串流中 60
echo "  ywp.running = $(ywp_running)（期望 false）"
ywp_why
udp_listening && echo "  UDP ${PORT} 在听！（不应该）" || echo "  UDP ${PORT} 没在听（正确）"
timed status
timed clients
timed idr
timed record m2-rec1.h265 3
sleep 4.5
timed snap m2-snap.h265
for i in $(seq 1 20); do [ -f "${DIR_M1}/m2-snap.h265" ] && break; sleep 0.5; done
ls -la "${DIR_M1}"/m2-* 2>/dev/null | awk '{print "  " $5, $9}'
echo "  （期望：m2-rec1.h265 和 m2-snap.h265 都在，没有 .tmp）"
timed inject 0.01
timed inject -1
timed pause
timed idr
timed resume
wait_state 串流中 60
timed restart
wait_state 串流中 60
stop_gw

echo "== 2. keys/ 权限不对：不起 YWP，PS5 会话照常"
if [ ! -f "${DIR}/keys/gateway.key" ]; then
	./yw-gateway keygen --dir "${DIR}" 2>&1 | sed 's/^/  /'
fi
if ! paired; then
	if [ -e "${DIR}/pair-${NAME}.txt" ]; then
		echo "  ${DIR}/pair-${NAME}.txt 已存在（上次的配对串还没删）：请用户确认后删掉再跑"
		exit 1
	fi
	./yw-gateway pair add "${NAME}" "${PUB}" --dir "${DIR}" 2>&1 | sed 's/^/  /'
fi
ls -la "${DIR}/keys" | awk 'NR > 1 {print "  " $1, $9}'
chmod 755 "${DIR}/keys"
start_gw "${DIR}"
wait_state 串流中 60
echo "  ywp.running = $(ywp_running)（期望 false）"
ywp_why
stop_gw
chmod 700 "${DIR}/keys"

echo "== 3. 有配对客户端：起 YWP"
start_gw "${DIR}"
wait_state 串流中 60
echo "  ywp.running = $(ywp_running)（期望 true）"
udp_listening && echo "  UDP ${PORT} 在听（正确）" || echo "  UDP ${PORT} 没在听！"
timed clients
ctl status | perl -ne 'print "  ywp: $1\n" if /"ywp":(\{.*\})\}$/'
echo "  配对串在 ${DIR}/pair-${NAME}.txt（600）；开发机导入：ssh ... 'cat ~/yw-gw-test/pair-${NAME}.txt' | yw-client pair import"

echo "== 4. 等客户端连进来（${WAIT_CLIENT} s）"
if [ "${WAIT_CLIENT}" -gt 0 ]; then
	got=0
	for i in $(seq 1 "${WAIT_CLIENT}"); do
		if ctl clients | grep -q '"current":{'; then got=1; break; fi
		sleep 1
	done
	if [ "${got}" = 1 ]; then
		echo "  客户端已接入：$(ctl clients)"
		for i in 1 2 3 4 5 6; do
			sleep 5
			ctl status | perl -ne 'my %v; for my $k (qw(forwarded key_forwarded w_forwarded tx_packets send_enobufs send_nobufs_dropped send_errors media_full key_pending idr_requested client_idr)) { $v{$k} = $1 if /"$k":(\d+)/ } print "  ", join(" ", map { "$_=$v{$_}" } sort keys %v), "\n"'
		done
		timed idr
		timed restart
		wait_state 串流中 60
		sleep 5
		echo "  重建后：$(ctl clients)"
	else
		echo "  ${WAIT_CLIENT} s 内没有客户端连进来"
	fi
fi

echo "== 5. quit（有客户端时先发 CLOSE(5)）"
stop_gw
ls "${DIR}"/*.tmp "${DIR_M1}"/*.tmp 2>/dev/null && echo "  有残留临时文件！" || echo "  没有残留临时文件（正确）"
for d in "${DIR_M1}" "${DIR}"; do
	echo "== 网关日志 ${d}（stderr，警告和 YWP、会话的关键事件）"
	grep -E " [EW] |\[(ywp|control|gateway|record|idr|watchdog)\]|会话结束|发起会话|串流中（" "${d}/stderr.log" | cut -c1-170 | tail -60
done
echo "== 日志里不应出现配对串（期望每个文件都是 0）"
grep -c 'ywp1-' "${DIR}/stderr.log" "${DIR}/gateway.jsonl" "${DIR_M1}/stderr.log" "${DIR_M1}/gateway.jsonl" 2>/dev/null | sed 's/^/  /'
