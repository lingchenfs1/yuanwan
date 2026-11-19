#!/bin/bash
# 许可证隔离检查：不含 chiaki 的五个 crate（ywp、yw-media、yw-serve、yw-client、yw-sim）
# 不能直接或间接依赖 yw-chiaki-sys（normal 和 build 依赖；dev 依赖不进发布产物，不查）。
# 用法：tools/check-license-isolation.sh
# 每个阶段交付前都要跑。cargo tree 只解析依赖图，不编译，不会触发 yw-chiaki-sys 的 build.rs。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATES=(ywp yw-media yw-serve yw-client yw-sim)
FORBIDDEN="yw-chiaki-sys"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

fail=0
for c in "${CRATES[@]}"; do
	out="${tmp}/${c}.txt"
	err="${tmp}/${c}.err"
	# --all-features：feature 打开的可选依赖也算；--locked：不改 Cargo.lock
	if ! cargo tree --manifest-path "${ROOT}/Cargo.toml" --locked -p "${c}" --all-features \
		-e normal,build --prefix none >"${out}" 2>"${err}"; then
		echo "失败：${c} 的 cargo tree 出错" >&2
		cat "${err}" >&2
		fail=1
		continue
	fi
	if grep -q "^${FORBIDDEN} " "${out}"; then
		echo "失败：${c} 依赖了 ${FORBIDDEN}，依赖路径：" >&2
		cargo tree --manifest-path "${ROOT}/Cargo.toml" --locked -p "${c}" --all-features \
			-e normal,build -i "${FORBIDDEN}" >&2 || true
		fail=1
	else
		n="$(sort -u "${out}" | wc -l | tr -d ' ')"
		echo "通过：${c}（${n} 个依赖，没有 ${FORBIDDEN}）"
	fi
done

if [ "${fail}" -ne 0 ]; then
	echo "许可证隔离检查没通过" >&2
	exit 1
fi
echo "许可证隔离检查通过"
