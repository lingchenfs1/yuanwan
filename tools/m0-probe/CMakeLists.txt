# yw-m0-probe：M0 实验工具。把 chiaki-ng 当子工程引入，只编核心库（不要 GUI、CLI、测试）。
cmake_minimum_required(VERSION 3.20)
project(yw_m0_probe C CXX)

get_filename_component(YW_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../.." ABSOLUTE)
set(CHIAKI_DIR "${YW_ROOT}/third_party/chiaki-ng")

macro(yw_force var value)
	set(${var} ${value} CACHE STRING "" FORCE)
endmacro()

# chiaki-ng 选项：只要核心库
yw_force(CHIAKI_ENABLE_TESTS OFF)
yw_force(CHIAKI_ENABLE_CLI OFF)
yw_force(CHIAKI_ENABLE_GUI OFF)
yw_force(CHIAKI_ENABLE_BOREALIS OFF)
yw_force(CHIAKI_ENABLE_SETSU OFF)
yw_force(CHIAKI_ENABLE_STEAMDECK_NATIVE OFF)
yw_force(CHIAKI_ENABLE_STEAM_SHORTCUT OFF)
yw_force(CHIAKI_LIB_ENABLE_OPUS OFF)       # 网关直通 Opus，不需要解码
yw_force(CHIAKI_ENABLE_SPEEX OFF)
yw_force(CHIAKI_ENABLE_FFMPEG_DECODER OFF)
yw_force(CHIAKI_ENABLE_PI_DECODER OFF)
yw_force(CHIAKI_USE_SYSTEM_JERASURE OFF)
yw_force(CHIAKI_USE_SYSTEM_NANOPB OFF)
yw_force(CHIAKI_USE_SYSTEM_CURL OFF)
yw_force(CHIAKI_LIB_JSONC_EXTERNAL_PROJECT ON)
yw_force(CHIAKI_LIB_MINIUPNPC_EXTERNAL_PROJECT ON)

# curl 子模块只给 PSN 打洞用，我们用不上：编最小的纯 HTTP 版，不找任何外部库
yw_force(CURL_ENABLE_SSL OFF)
yw_force(CURL_USE_LIBPSL OFF)
yw_force(CURL_USE_LIBSSH2 OFF)
yw_force(CURL_USE_GSSAPI OFF)
yw_force(CURL_BROTLI OFF)
yw_force(CURL_ZSTD OFF)
yw_force(CURL_ZLIB OFF)
yw_force(USE_NGHTTP2 OFF)
yw_force(USE_LIBIDN2 OFF)
yw_force(USE_APPLE_IDN OFF)

# chiaki-ng 在 json-c、miniupnpc 都走外部工程时，lib/ 作用域里没调用过 find_package(PkgConfig)，
# 找 libevent 时 PKG_CONFIG_VERSION 为空会报错。这里先找一次，子目录会继承这些变量。
find_package(PkgConfig REQUIRED)

add_subdirectory("${CHIAKI_DIR}" chiaki EXCLUDE_FROM_ALL)

add_executable(yw-m0-probe main.c)
target_link_libraries(yw-m0-probe PRIVATE chiaki-lib)
