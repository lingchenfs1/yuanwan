#!/bin/bash
# 拉取固定版本的 chiaki-ng（v1.10.1）和核心库需要的子模块，再按顺序打上 third_party/patches/ 里的补丁。
# 已经打过时，会在临时目录里重放「原始文件 + 全部补丁」并与当前源码比对，一致才算已打好；
# 如果有补丁以外的改动，报错退出，不会去动它。
# --check：只校验源码和补丁是否一致（不拉取、不打补丁），供 build-shim-macos.sh 在编译前调用。
set -euo pipefail
CHECK_ONLY=0
if [ "${1:-}" = "--check" ]; then
	CHECK_ONLY=1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="${ROOT}/third_party/chiaki-ng"
TAG=v1.10.1
EXPECT_COMMIT=f30da0aaeb96fc0026cc03683153f682352e7d7c
PATCHES=("${ROOT}"/third_party/patches/*.patch)

if [ ! -d "${DIR}/.git" ]; then
	if [ "${CHECK_ONLY}" = 1 ]; then
		echo "没有 chiaki-ng 源码，先运行 tools/fetch-chiaki.sh" >&2
		exit 1
	fi
	git clone --depth 1 --branch "${TAG}" https://github.com/chiaki-ng/chiaki-ng.git "${DIR}"
fi
got="$(git -C "${DIR}" rev-parse HEAD)"
if [ "${got}" != "${EXPECT_COMMIT}" ]; then
	echo "chiaki-ng 提交不对：期望 ${EXPECT_COMMIT}，实际 ${got}" >&2
	exit 1
fi
if [ "${CHECK_ONLY}" = 0 ]; then
	git -C "${DIR}" submodule update --init --depth 1 \
		third-party/nanopb third-party/jerasure third-party/gf-complete third-party/curl
fi

if git -C "${DIR}" diff --quiet; then
	if [ "${CHECK_ONLY}" = 1 ]; then
		echo "chiaki-ng 还没打补丁，先运行 tools/fetch-chiaki.sh" >&2
		exit 1
	fi
	for p in "${PATCHES[@]}"; do
		git -C "${DIR}" apply "${p}"
		echo "打上：$(basename "${p}")"
	done
	exit 0
fi

# 已有改动：重放全部补丁再比对
check="$(mktemp -d)"
trap 'rm -rf "${check}"' EXIT
changed="$(git -C "${DIR}" diff --name-only)"
patched="$(grep -h '^+++ b/' "${PATCHES[@]}" | sed 's|^+++ b/||' | sort -u)"
if [ "$(echo "${changed}" | sort -u)" != "${patched}" ]; then
	echo "chiaki-ng 里改动的文件和补丁涉及的文件不一致（改了源码要用 tools/export-chiaki-patch.sh 导出补丁）：" >&2
	diff <(echo "${changed}" | sort -u) <(echo "${patched}") >&2 || true
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
)
while IFS= read -r f; do
	if ! cmp -s "${check}/${f}" "${DIR}/${f}"; then
		echo "${f} 与「原始文件 + 全部补丁」不一致：改了源码要用 tools/export-chiaki-patch.sh 导出补丁，否则请先检查。" >&2
		exit 1
	fi
done <<< "${changed}"
echo "全部 ${#PATCHES[@]} 个补丁都已打上，与源码一致。"
