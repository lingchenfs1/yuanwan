#!/bin/bash
# 在网关机上由用户执行（需要 sudo 密码）：bash ~/yw-gw/deploy/install.sh
# 把 yw-gateway 装成 LaunchDaemon 并立即启动。只需要做一次；以后更新程序不用 sudo（用开发机上的 tools/deploy-gateway.sh）。
# 以运行本脚本的用户身份运行网关（不是 root）。PS5 地址写在 ~/yw-gw/gateway.conf（一行 host=地址）。
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
LABEL=com.yw.gateway
PLIST=/Library/LaunchDaemons/${LABEL}.plist
GW_USER="$(id -un)"
GW_HOME="${HOME}"

test "${GW_USER}" != root || { echo "请用普通用户运行（脚本里会自己调用 sudo）" >&2; exit 1; }
test -x "${GW_HOME}/yw-gw/bin/yw-gateway" || { echo "缺 ${GW_HOME}/yw-gw/bin/yw-gateway，先从开发机部署程序" >&2; exit 1; }
test -f "${GW_HOME}/.config/yw/ps5.cred" || { echo "缺 PS5 凭据 ${GW_HOME}/.config/yw/ps5.cred" >&2; exit 1; }
grep -qE '^ *host *= *[^ ]+' "${GW_HOME}/yw-gw/gateway.conf" 2>/dev/null || { echo "缺 ${GW_HOME}/yw-gw/gateway.conf（写一行 host=PS5 地址）" >&2; exit 1; }

RENDERED="$(mktemp -t yw-gateway-plist)"
sed -e "s|__USER__|${GW_USER}|g" -e "s|__HOME__|${GW_HOME}|g" "${HERE}/${LABEL}.plist.in" > "${RENDERED}"
plutil -lint "${RENDERED}"

# 先 enable：服务以前被 disable 过时 bootstrap 会失败
sudo launchctl enable "system/${LABEL}"

if sudo launchctl print "system/${LABEL}" >/dev/null 2>&1; then
	echo "已经安装过，先卸载旧的（网关会正常结束 PS5 会话，最多等 40 秒）"
	sudo launchctl bootout "system/${LABEL}" 2>/dev/null || true
	for i in $(seq 1 40); do
		sudo launchctl print "system/${LABEL}" >/dev/null 2>&1 || break
		sleep 1
	done
	if sudo launchctl print "system/${LABEL}" >/dev/null 2>&1; then
		echo "40 秒后旧服务还没卸载，先不安装。可以稍后重跑本脚本。" >&2
		exit 1
	fi
fi
sudo install -m 644 -o root -g wheel "${RENDERED}" "${PLIST}"
mv "${RENDERED}" "${GW_HOME}/.Trash/" 2>/dev/null || true
if ! sudo launchctl bootstrap system "${PLIST}"; then
	echo "bootstrap 失败。当前状态：" >&2
	sudo launchctl print "system/${LABEL}" >&2 || true
	exit 1
fi
sleep 3
sudo launchctl print "system/${LABEL}" | grep -E "state =|pid =|last exit" || true
echo "已安装。查看状态：echo status | nc -U ${GW_HOME}/yw-gw/control.sock"
