#!/bin/bash
# 开发机脚本的公共配置：读 tools/local.env（不进仓库），给出 SSH/SCP 参数。
# 用法：在脚本里 . "$(dirname "$0")/env.sh"
YW_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "${YW_ROOT}/tools/local.env" ]; then
	# shellcheck disable=SC1091
	. "${YW_ROOT}/tools/local.env"
fi
if [ -z "${YW_GW_SSH:-}" ]; then
	echo "缺网关机登录配置：把 tools/local.env.example 复制成 tools/local.env 并填好" >&2
	exit 1
fi
YW_SSH_KEY="${YW_SSH_KEY:-${HOME}/.ssh/id_ed25519}"
YW_GW_IP="${YW_GW_IP:-${YW_GW_SSH#*@}}"
YW_BIND_IF="${YW_BIND_IF:-en0}"
YW_SSH=(ssh -i "${YW_SSH_KEY}" -o IdentitiesOnly=yes -o BatchMode=yes)
YW_SCP=(scp -q -i "${YW_SSH_KEY}" -o IdentitiesOnly=yes -o BatchMode=yes)
