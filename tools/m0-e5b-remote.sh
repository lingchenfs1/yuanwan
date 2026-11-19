#!/bin/bash
# T12：模拟「真正断网」后 PS5 多久释放会话。在网关机上运行：ssh ... 'bash -s' < tools/m0-e5b-remote.sh
# 做法：会话 A 出画面后用 SIGSTOP 冻结（连接不会被内核关闭，PS5 收不到任何断开信号），
# 然后每秒尝试建新会话 B，直到成功；最后再结束 A。
set -u
HOST="${YW_PS5:-$( { sed -n 's/^ *host *= *//p' ~/yw-gw/gateway.conf 2>/dev/null || true; } | head -1)}"  # PS5 地址：和网关一样取 ~/yw-gw/gateway.conf，可用 YW_PS5 覆盖
HOST="${HOST:?网关机上没有 ~/yw-gw/gateway.conf（host=PS5 地址），也没设 YW_PS5}"
cd ~/yw-m0 || exit 1
now() { perl -MTime::HiRes=time -e 'printf "%.1f", time'; }

s=$(./yw-m0-probe status "${HOST}" | head -1)
case "${s}" in
	*待机*)
		./yw-m0-probe wakeup "${HOST}" > /dev/null
		for i in $(seq 1 60); do
			case "$(./yw-m0-probe status "${HOST}" 2>&1 | head -1)" in *就绪*) break;; esac
			sleep 1
		done
		echo "已唤醒"; sleep 5
		;;
esac

rm -f e5b-a.log
./yw-m0-probe stream "${HOST}" --duration 600 --log e5b-a.log > /dev/null 2>&1 &
A=$!
for i in $(seq 1 40); do grep -q "首帧" e5b-a.log 2>/dev/null && break; sleep 0.5; done
grep -o "首帧：.*" e5b-a.log || { echo "会话 A 没有出画面，放弃"; kill "${A}"; exit 1; }
sleep 5
kill -STOP "${A}"
t0=$(now)
echo "已冻结会话 A（模拟断网）"
for i in $(seq 1 120); do
	r=$(./yw-m0-probe stream "${HOST}" --duration 2 --log e5b-try.log 2>&1 | grep -E "首帧|会话结束：[^S]" | head -1 | cut -c7-90)
	t=$(perl -e "printf '%.1f', $(now) - ${t0}")
	echo "${t}s：${r}"
	case "${r}" in *首帧*) break;; esac
	sleep 1
done
kill -9 "${A}" 2>/dev/null
wait "${A}" 2>/dev/null
echo "会话 A 已结束"
