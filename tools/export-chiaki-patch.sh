#!/bin/bash
# 把 third_party/chiaki-ng 里「已有补丁之外」的改动导出成一个新补丁。
# 用法：tools/export-chiaki-patch.sh 0005-yw-xxx
# 做法：在临时目录里重放「原始文件 + 已有全部补丁」，再和当前源码比对，差异写成 third_party/patches/<名字>.patch。
# 导出后用 tools/fetch-chiaki.sh --check 确认一致。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="${ROOT}/third_party/chiaki-ng"
NAME="${1:?用法：$0 <编号-名字>，例如 0005-yw-stop-cancel}"
OUT="${ROOT}/third_party/patches/${NAME%.patch}.patch"
if [ -e "${OUT}" ]; then
	echo "${OUT} 已存在" >&2
	exit 1
fi
PATCHES=("${ROOT}"/third_party/patches/*.patch)

check="$(mktemp -d)"
trap 'rm -rf "${check}"' EXIT
changed="$(git -C "${DIR}" diff --name-only)"
if [ -z "${changed}" ]; then
	echo "chiaki-ng 没有任何改动" >&2
	exit 1
fi
(
	cd "${check}"
	git init -q .
	while IFS= read -r f; do
		mkdir -p "$(dirname "${f}")"
		git -C "${DIR}" show "HEAD:${f}" > "${f}"
	done <<< "${changed}"
	for p in "${PATCHES[@]}"; do
		git apply "${p}"
	done
	git add -A
	git -c user.name=yw -c user.email=yw@localhost commit -q -m base
	while IFS= read -r f; do
		cp "${DIR}/${f}" "${f}"
	done <<< "${changed}"
	git diff > "${OUT}"
)
if [ ! -s "${OUT}" ]; then
	mv "${OUT}" "${check}/empty.patch"
	echo "和已有补丁相比没有新改动" >&2
	exit 1
fi
echo "已导出 ${OUT}："
grep '^+++ b/' "${OUT}" | sed 's|^+++ b/|  |'
