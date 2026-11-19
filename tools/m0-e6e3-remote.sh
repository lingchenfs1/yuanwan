#!/bin/bash
# E6（唤醒计时）+ 补丁 0004 验证 + E3（LaunchSpec mtu）。在网关机上运行：ssh ... 'bash -s' < tools/m0-e6e3-remote.sh
set -u
HOST="${YW_PS5:-$( { sed -n 's/^ *host *= *//p' ~/yw-gw/gateway.conf 2>/dev/null || true; } | head -1)}"  # PS5 地址：和网关一样取 ~/yw-gw/gateway.conf，可用 YW_PS5 覆盖
HOST="${HOST:?网关机上没有 ~/yw-gw/gateway.conf（host=PS5 地址），也没设 YW_PS5}"
cd ~/yw-m0 || exit 1
now() { perl -MTime::HiRes=time -e 'printf "%.1f", time'; }

# 带冷却重试的短会话
run_session() { # run_session 日志名 时长 [额外参数...]
	local name="$1" dur="$2"; shift 2
	for attempt in $(seq 1 12); do
		./yw-m0-probe stream "${HOST}" --duration "${dur}" --log "${name}.log" "$@" > /dev/null 2>&1
		if grep -q "首帧" "${name}.log"; then
			echo "${name}：完成（第 ${attempt} 次）$(grep -o '首帧：.*' "${name}.log" | head -1)"
			return 0
		fi
		echo "${name}：第 ${attempt} 次没连上（$(grep -o '会话结束：.*' "${name}.log" | head -1)），5 秒后重试"
		sleep 5
	done
	return 1
}

# ---- E6：从待机唤醒到就绪 ----
state=$(./yw-m0-probe status "${HOST}" | head -1)
echo "唤醒前：${state}"
case "${state}" in
	*待机*)
		t0=$(now)
		./yw-m0-probe wakeup "${HOST}" > /dev/null
		for i in $(seq 1 90); do
			s=$(./yw-m0-probe status "${HOST}" 2>&1 | head -1)
			case "$s" in *就绪*) echo "E6：唤醒后 $(perl -e "printf '%.1f', $(now) - ${t0}") s 就绪"; break;; esac
			sleep 1
		done
		sleep 5
		;;
esac

# ---- 补丁 0004 验证：默认 mtu，15 Mbps，30 s ----
run_session t26 30
sleep 10
# ---- E3：覆盖 LaunchSpec 的 mtu ----
run_session e3-1231 30 --ls-mtu 1231
sleep 10
run_session e3-1000 30 --ls-mtu 1000

echo "==== 汇总"
for f in t26 e3-1231 e3-1000; do
	echo "--- ${f}"
	grep -o '\[yw-ls\].*' "${f}.log" | head -1
	echo "data ack 报错 $(grep -c 'data ack with size' "${f}.log")；快速重传 $(grep -c 'fast re-sending' "${f}.log")；重复数据丢弃 $(grep -c 'dropping data' "${f}.log")；未处理 ctrl 消息 $(grep -c 'unhandled message type' "${f}.log")；放弃重传 $(grep -c -i 'giving up' "${f}.log")"
	grep -o 'unhandled message type [^,]*, size [^ ]*' "${f}.log" | sort | uniq -c
	/usr/bin/python3 -I m0-analyze.py "${f}.log" | grep -E '^\| (开始|注入)'
	grep -o '\[yw-run\] 汇总.*' "${f}.log"
done
