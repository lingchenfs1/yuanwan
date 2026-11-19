#!/bin/bash
# 在 macOS（Apple 芯片）上把 chiaki-ng 核心库需要的第三方依赖编成静态库，
# 最低系统版本 13.0，这样编出的程序拷到别的 Mac（如网关机 macOS 15）也能直接运行，
# 不依赖 Homebrew 的动态库。
#
# 产物：third_party/deps/macos-arm64/{include,lib}
# 用法：tools/build-deps-macos.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEPS="${ROOT}/third_party/deps"
PREFIX="${DEPS}/macos-arm64"
WORK="${DEPS}/work"
VENV="${DEPS}/venv"
JOBS="$(sysctl -n hw.ncpu)"

export MACOSX_DEPLOYMENT_TARGET=13.0

OPENSSL_VER=3.5.9
OPENSSL_SHA256=603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a
OPENSSL_URL="https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VER}/openssl-${OPENSSL_VER}.tar.gz"

LIBEVENT_VER=2.1.13-stable
LIBEVENT_SHA256=f7e9383b8c0baa81b687e5b5eecc01beefaf1b19b64151d95ed61647fe7a315c
LIBEVENT_URL="https://github.com/libevent/libevent/releases/download/release-${LIBEVENT_VER}/libevent-${LIBEVENT_VER}.tar.gz"

mkdir -p "${PREFIX}" "${WORK}"

fetch() { # fetch <url> <sha256> <输出文件>
	local url="$1" sha="$2" out="$3"
	if [ ! -f "${out}" ]; then
		curl -fL --retry 3 -o "${out}.part" "${url}"
		mv "${out}.part" "${out}"
	fi
	local got
	got="$(shasum -a 256 "${out}" | awk '{print $1}')"
	if [ "${got}" != "${sha}" ]; then
		echo "校验失败：${out}，期望 ${sha}，实际 ${got}" >&2
		exit 1
	fi
}

# ---- OpenSSL（只要 libcrypto/libssl 静态库和头文件）----
if [ ! -f "${PREFIX}/lib/libcrypto.a" ]; then
	fetch "${OPENSSL_URL}" "${OPENSSL_SHA256}" "${WORK}/openssl-${OPENSSL_VER}.tar.gz"
	rm -rf "${WORK}/openssl-${OPENSSL_VER}"
	tar -xzf "${WORK}/openssl-${OPENSSL_VER}.tar.gz" -C "${WORK}"
	(
		cd "${WORK}/openssl-${OPENSSL_VER}"
		./Configure darwin64-arm64-cc no-shared no-tests no-docs no-apps \
			--prefix="${PREFIX}" --libdir=lib "-mmacosx-version-min=${MACOSX_DEPLOYMENT_TARGET}"
		make -j"${JOBS}" build_libs > "${WORK}/openssl-build.log" 2>&1 || { tail -30 "${WORK}/openssl-build.log"; exit 1; }
		make install_dev > "${WORK}/openssl-install.log" 2>&1 || { tail -30 "${WORK}/openssl-install.log"; exit 1; }
	)
	echo "OpenSSL ${OPENSSL_VER} 完成"
else
	echo "OpenSSL 已存在，跳过"
fi

# ---- libevent（静态，不带 TLS）----
if [ ! -f "${PREFIX}/lib/libevent.a" ]; then
	fetch "${LIBEVENT_URL}" "${LIBEVENT_SHA256}" "${WORK}/libevent-${LIBEVENT_VER}.tar.gz"
	rm -rf "${WORK}/libevent-${LIBEVENT_VER}" "${WORK}/libevent-build"
	tar -xzf "${WORK}/libevent-${LIBEVENT_VER}.tar.gz" -C "${WORK}"
	cmake -S "${WORK}/libevent-${LIBEVENT_VER}" -B "${WORK}/libevent-build" -G Ninja \
		-DCMAKE_BUILD_TYPE=Release \
		-DCMAKE_POLICY_VERSION_MINIMUM=3.5 \
		-DCMAKE_OSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET}" \
		-DCMAKE_INSTALL_PREFIX="${PREFIX}" \
		-DEVENT__LIBRARY_TYPE=STATIC \
		-DEVENT__DISABLE_OPENSSL=ON -DEVENT__DISABLE_MBEDTLS=ON \
		-DEVENT__DISABLE_TESTS=ON -DEVENT__DISABLE_REGRESS=ON \
		-DEVENT__DISABLE_SAMPLES=ON -DEVENT__DISABLE_BENCHMARK=ON \
		> "${WORK}/libevent-config.log" 2>&1 || { tail -30 "${WORK}/libevent-config.log"; exit 1; }
	cmake --build "${WORK}/libevent-build" --target install > "${WORK}/libevent-build.log" 2>&1 || { tail -30 "${WORK}/libevent-build.log"; exit 1; }
	echo "libevent ${LIBEVENT_VER} 完成"
else
	echo "libevent 已存在，跳过"
fi

# ---- nanopb 代码生成器要用的 Python 环境 ----
if [ ! -x "${VENV}/bin/python3" ]; then
	python3 -m venv "${VENV}"
fi
if ! "${VENV}/bin/python3" -c "import google.protobuf" 2>/dev/null; then
	"${VENV}/bin/pip" install --quiet protobuf
fi
echo "Python 环境：$("${VENV}/bin/python3" -c 'import google.protobuf as p; print("protobuf", p.__version__)')"

echo "依赖目录：${PREFIX}"
ls "${PREFIX}/lib"
