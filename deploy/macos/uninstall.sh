#!/bin/bash
# 在网关机上由用户执行（需要 sudo 密码）：bash ~/yw-gw/deploy/uninstall.sh
# 停止并卸载 LaunchDaemon。网关收到 SIGTERM 会正常结束 PS5 会话。
# plist 文件移到当前用户的废纸篓，不直接删除；程序和日志留在 ~/yw-gw。
set -euo pipefail
PLIST=/Library/LaunchDaemons/com.yw.gateway.plist
sudo launchctl bootout system/com.yw.gateway 2>/dev/null || echo "服务没有在运行"
if [ -f "${PLIST}" ]; then
	sudo mv "${PLIST}" "${HOME}/.Trash/com.yw.gateway.plist.$(date +%Y%m%d%H%M%S)"
	echo "已把 ${PLIST} 移到废纸篓"
fi
