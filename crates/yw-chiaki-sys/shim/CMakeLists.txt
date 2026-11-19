# yw_shim：libchiaki 外壳。由 tools/build-shim-macos.sh 调用，产物合并成 build/shim-macos/libyw_all.a 给 Rust 链接。
cmake_minimum_required(VERSION 3.20)
project(yw_shim C CXX)

get_filename_component(YW_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/../../.." ABSOLUTE)
set(CHIAKI_DIR "${YW_ROOT}/third_party/chiaki-ng")

macro(yw_force var value)
	set(${var} ${value} CACHE STRING "" FORCE)
endmacro()

# chiaki-ng 选项：只要核心库（与 tools/m0-probe 一致）
yw_force(CHIAKI_ENABLE_TESTS OFF)
yw_force(CHIAKI_ENABLE_CLI OFF)
yw_force(CHIAKI_ENABLE_GUI OFF)
yw_force(CHIAKI_ENABLE_BOREALIS OFF)
yw_force(CHIAKI_ENABLE_SETSU OFF)
yw_force(CHIAKI_ENABLE_STEAMDECK_NATIVE OFF)
yw_force(CHIAKI_ENABLE_STEAM_SHORTCUT OFF)
yw_force(CHIAKI_LIB_ENABLE_OPUS OFF)
yw_force(CHIAKI_ENABLE_SPEEX OFF)
yw_force(CHIAKI_ENABLE_FFMPEG_DECODER OFF)
yw_force(CHIAKI_ENABLE_PI_DECODER OFF)
yw_force(CHIAKI_USE_SYSTEM_JERASURE OFF)
yw_force(CHIAKI_USE_SYSTEM_NANOPB OFF)
yw_force(CHIAKI_USE_SYSTEM_CURL OFF)
yw_force(CHIAKI_LIB_JSONC_EXTERNAL_PROJECT ON)
yw_force(CHIAKI_LIB_MINIUPNPC_EXTERNAL_PROJECT ON)
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

# 见 tools/m0-probe/CMakeLists.txt：先找一次 PkgConfig，子目录会继承 PKG_CONFIG_VERSION
find_package(PkgConfig REQUIRED)

add_subdirectory("${CHIAKI_DIR}" chiaki EXCLUDE_FROM_ALL)

add_library(yw_shim STATIC yw_shim.c)
target_link_libraries(yw_shim PUBLIC chiaki-lib)

# 链接检查用的小程序：让 CMake 算出完整的链接行，构建脚本据此收集所有静态库
add_executable(yw_shim_linkcheck linkcheck.c)
target_link_libraries(yw_shim_linkcheck PRIVATE yw_shim)
