#!/bin/bash
# 从开发机把 yw-gateway 部署到网关机的 ~/yw-gw/bin/，并同步 deploy/macos/ 到 ~/yw-gw/deploy/。
# 用法：tools/deploy-gateway.sh [--no-restart]
# 先传成 yw-gateway.new，在网关机上校验签名后 mv -f 原子替换（新 inode），不原地覆盖正在运行的程序。
# 网关以 LaunchDaemon 运行时，替换后发 quit，KeepAlive 会用新程序重启；--no-restart 只替换不重启。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
. "${ROOT}/tools/env.sh"   # 网关机登录在 tools/local.env（不进仓库）
BIN="${ROOT}/target/release/yw-gateway"
GW="${YW_GW_SSH}"
RESTART=1
[ "${1:-}" = "--no-restart" ] && RESTART=0

gwssh() { "${YW_SSH[@]}" "${GW}" "$@"; }
gwscp() { "${YW_SCP[@]}" "$@"; }

test -x "${BIN}" || { echo "缺 ${BIN}，先 MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release -p yw-gateway" >&2; exit 1; }
codesign -v "${BIN}"
NEW_SHA="$(shasum -a 256 "${BIN}" | cut -c1-16)"

echo "== 网关机上现在的版本"
gwssh 'mkdir -p ~/yw-gw/bin ~/yw-gw/deploy && if [ -f ~/yw-gw/bin/yw-gateway ]; then shasum -a 256 ~/yw-gw/bin/yw-gateway | cut -c1-16; else echo "（还没有）"; fi'
echo "== 要部署的版本 ${NEW_SHA}"

gwscp "${BIN}" "${GW}:yw-gw/bin/yw-gateway.new"
gwscp "${ROOT}"/deploy/macos/com.yw.gateway.plist.in "${ROOT}"/deploy/macos/install.sh "${ROOT}"/deploy/macos/uninstall.sh "${GW}:yw-gw/deploy/"
gwssh 'set -e; cd ~/yw-gw/bin; codesign -v yw-gateway.new; chmod 755 yw-gateway.new; mv -f yw-gateway.new yw-gateway; echo "已替换：$(shasum -a 256 yw-gateway | cut -c1-16)"'

if [ "${RESTART}" = 0 ]; then
	echo "没有重启（--no-restart）"
	exit 0
fi
# 只有 LaunchDaemon 在跑时才发 quit（KeepAlive 会拉起新程序）；手动在 SSH 里跑的测试实例不归这里管
if gwssh 'launchctl print system/com.yw.gateway >/dev/null 2>&1'; then
	echo "== 让 LaunchDaemon 用新程序重启"
	gwssh 'echo quit | nc -U ~/yw-gw/control.sock || true'
	for i in $(seq 1 30); do
		sleep 2
		if out="$(gwssh 'echo status | nc -U ~/yw-gw/control.sock 2>/dev/null')" && [ -n "${out}" ]; then
			echo "${out}"
			exit 0
		fi
	done
	echo "60 秒内控制口没有回应，查看 ~/yw-gw/stderr.log 和 gateway.jsonl" >&2
	exit 1
else
	echo "LaunchDaemon 还没装：请在网关机上执行 bash ~/yw-gw/deploy/install.sh（需要 sudo）"
fi
