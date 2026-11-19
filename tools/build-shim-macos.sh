#!/bin/bash
# 编译 yw_shim（libchiaki 外壳）并把它和全部依赖的静态库合并成 build/shim-macos/libyw_all.a，供 Rust 链接。
# 前提：tools/fetch-chiaki.sh、tools/build-deps-macos.sh 已运行。
# 输出最后一行是需要额外链接的系统 framework 列表（逗号分隔），给 build.rs 用。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${ROOT}/third_party/deps/macos-arm64"
VENV="${ROOT}/third_party/deps/venv"
BUILD="${ROOT}/build/shim-macos"
OUT="${BUILD}/libyw_all.a"

# 源码必须正好是「原始 + third_party/patches 全部补丁」，否则编出来的不是补丁描述的代码
"${ROOT}/tools/fetch-chiaki.sh" --check >&2 || exit 1

if [ ! -f "${PREFIX}/lib/libcrypto.a" ]; then
	echo "缺依赖，先运行 tools/build-deps-macos.sh" >&2
	exit 1
fi
PROTOC="$(command -v protoc || true)"
if [ -z "${PROTOC}" ]; then
	echo "缺 protoc（编译期工具），先 brew install protobuf" >&2
	exit 1
fi
export PKG_CONFIG_LIBDIR="${PREFIX}/lib/pkgconfig"
export PKG_CONFIG_PATH=""
export MACOSX_DEPLOYMENT_TARGET=13.0

mkdir -p "${BUILD}"
cmake -S "${ROOT}/crates/yw-chiaki-sys/shim" -B "${BUILD}/cmake" -G Ninja \
	-DCMAKE_BUILD_TYPE=RelWithDebInfo \
	-DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
	-DCMAKE_PREFIX_PATH="${PREFIX}" \
	-DCMAKE_IGNORE_PREFIX_PATH="/opt/homebrew;/usr/local" \
	-DOPENSSL_ROOT_DIR="${PREFIX}" \
	-DOPENSSL_USE_STATIC_LIBS=TRUE \
	-DPYTHON_EXECUTABLE="${VENV}/bin/python3" \
	-DPROTOC="${PROTOC}" \
	-DPKG_CONFIG_EXECUTABLE="$(command -v pkg-config)" \
	> "${BUILD}/config.log" 2>&1 || { tail -40 "${BUILD}/config.log" >&2; exit 1; }
cmake --build "${BUILD}/cmake" --target yw_shim_linkcheck > "${BUILD}/build.log" 2>&1 || { tail -60 "${BUILD}/build.log" >&2; exit 1; }

# 从 ninja 的链接命令里取出全部 .a（按出现顺序去重），以及 -framework
cmd="$(cd "${BUILD}/cmake" && ninja -t commands yw_shim_linkcheck | tail -1)"
libs=()
frameworks=()
prev=""
for tok in ${cmd}; do
	if [ "${prev}" = "-framework" ]; then
		frameworks+=("${tok}")
	elif [[ "${tok}" == *.a ]]; then
		[[ "${tok}" = /* ]] || tok="${BUILD}/cmake/${tok}"
		seen=0
		for x in "${libs[@]+"${libs[@]}"}"; do [ "${x}" = "${tok}" ] && seen=1; done
		[ "${seen}" = 0 ] && libs+=("${tok}")
	elif [[ "${tok}" == -l* ]]; then
		# -levent 之类：在依赖目录里找对应的 .a
		name="${tok#-l}"
		if [ -f "${PREFIX}/lib/lib${name}.a" ]; then
			libs+=("${PREFIX}/lib/lib${name}.a")
		fi
	fi
	prev="${tok}"
done
rm -f "${OUT}"
libtool -static -no_warning_for_no_symbols -o "${OUT}" "${libs[@]}"
echo "已合并 ${#libs[@]} 个静态库：$(for l in "${libs[@]}"; do basename "${l}"; done | tr '\n' ' ')" >&2
ls -la "${OUT}" >&2
# 去重后的 framework 列表
printf '%s\n' "${frameworks[@]+"${frameworks[@]}"}" | sort -u | paste -sd, -
