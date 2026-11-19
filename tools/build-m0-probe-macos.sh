#!/bin/bash
# 编译 M0 实验工具 yw-m0-probe（macOS，Apple 芯片）。
# 先跑 tools/build-deps-macos.sh 准备依赖。产物：build/m0-probe-macos/yw-m0-probe
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PREFIX="${ROOT}/third_party/deps/macos-arm64"
VENV="${ROOT}/third_party/deps/venv"
BUILD="${ROOT}/build/m0-probe-macos"

if [ ! -f "${PREFIX}/lib/libcrypto.a" ]; then
	echo "缺依赖，先运行 tools/build-deps-macos.sh" >&2
	exit 1
fi
PROTOC="$(command -v protoc || true)"
if [ -z "${PROTOC}" ]; then
	echo "缺 protoc（编译期工具），先 brew install protobuf" >&2
	exit 1
fi

# 只让 pkg-config 看见我们自己的依赖目录，避免链接到 Homebrew 的动态库
export PKG_CONFIG_LIBDIR="${PREFIX}/lib/pkgconfig"
export PKG_CONFIG_PATH=""

mkdir -p "$(dirname "${BUILD}")"
cmake -S "${ROOT}/tools/m0-probe" -B "${BUILD}" -G Ninja \
	-DCMAKE_BUILD_TYPE=RelWithDebInfo \
	-DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
	-DCMAKE_PREFIX_PATH="${PREFIX}" \
	-DCMAKE_IGNORE_PREFIX_PATH="/opt/homebrew;/usr/local" \
	-DOPENSSL_ROOT_DIR="${PREFIX}" \
	-DOPENSSL_USE_STATIC_LIBS=TRUE \
	-DPYTHON_EXECUTABLE="${VENV}/bin/python3" \
	-DPROTOC="${PROTOC}" \
	-DPKG_CONFIG_EXECUTABLE="$(command -v pkg-config)" \
	> "${BUILD}.config.log" 2>&1 || { mkdir -p "$(dirname "${BUILD}")"; tail -40 "${BUILD}.config.log"; exit 1; }

cmake --build "${BUILD}" --target yw-m0-probe > "${BUILD}.build.log" 2>&1 || { tail -60 "${BUILD}.build.log"; exit 1; }

BIN="${BUILD}/yw-m0-probe"
echo "编译完成：${BIN}"
echo "动态库依赖（应当只有系统库）："
otool -L "${BIN}" | tail -n +2
echo "最低系统版本："
otool -l "${BIN}" | grep -A3 LC_BUILD_VERSION | grep -E "minos|sdk"
