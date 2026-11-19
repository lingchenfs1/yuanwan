// SPDX-License-Identifier: AGPL-3.0-only
// yw_shim：libchiaki 的薄外壳。只暴露「不透明句柄 + 回调」，Rust 侧手写 FFI 声明即可，
// 不需要 bindgen，也不需要知道 ChiakiSession 等结构体的布局。
#ifndef YW_SHIM_H
#define YW_SHIM_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct yw_session yw_session;

// 日志级别与 chiaki 一致（位掩码）
enum
{
	YW_LOG_ERROR = 1 << 0,
	YW_LOG_WARNING = 1 << 1,
	YW_LOG_INFO = 1 << 2,
	YW_LOG_VERBOSE = 1 << 3,
	YW_LOG_DEBUG = 1 << 4,
	YW_LOG_ALL = (1 << 5) - 1,
};

// 事件类型
enum
{
	YW_EV_CONNECTED = 1,
	YW_EV_QUIT = 2,          // code = 退出原因（chiaki 的 ChiakiQuitReason），text = 附带说明（可能为 NULL）
	YW_EV_LOGIN_PIN = 3,     // code = 1 表示上次的 PIN 不对
	YW_EV_RUMBLE = 4,        // code = (left << 8) | right
	YW_EV_TRIGGER_EFFECTS = 5,
	YW_EV_FEC_FAILURE = 6,
	YW_EV_OTHER = 99,        // code = chiaki 的事件类型
};

typedef struct
{
	void *user;
	void (*on_log)(void *user, int level, const char *msg);
	void (*on_event)(void *user, int type, int code, const char *text);
	// 返回 0 表示网关没收下这一帧（比如队列满）。video_cb 把它原样交还给 chiaki：
	// chiaki 就不把这一帧记作参考帧，下一帧开始时向 PS5 报 CORRUPTFRAME，引用它的 P 帧由 chiaki 改参考或丢弃
	int (*on_video)(void *user, const uint8_t *buf, size_t size, int32_t frames_lost, int recovered);
	void (*on_audio_header)(void *user, uint32_t channels, uint32_t bits, uint32_t rate, uint32_t frame_size);
	void (*on_audio)(void *user, const uint8_t *buf, size_t size);
	void (*on_haptics)(void *user, const uint8_t *buf, size_t size);
} yw_callbacks;

typedef struct
{
	const char *host;          // IPv4 地址或主机名
	uint8_t regist_key[16];    // 凭据里的 regist_key 原样 16 字节
	uint8_t morning[16];       // 凭据里的 rp_key
	uint32_t height;           // 360 / 540 / 720 / 1080
	uint32_t fps;              // 30 / 60
	uint32_t bitrate_kbps;     // 0 = 按分辨率的默认值
	int codec;                 // 0 H.264，1 H.265，2 H.265 HDR
	double packet_loss_max;    // 上报给 PS5 的丢包率上限
	uint32_t ls_mtu;           // 非 0 时覆盖 LaunchSpec 的 mtu（补丁 0003）
	uint32_t ls_rtt_ms;        // 非 0 时覆盖 LaunchSpec 的 rtt
	uint32_t console_pin;      // PS5 设了登录密码时的 4 位 PIN，0 表示没有
	uint32_t log_mask;         // 交给 on_log 的日志级别
} yw_session_params;

typedef struct
{
	uint32_t buttons;          // chiaki 的按键位（CROSS = 1 << 0 …… PS = 1 << 15）
	uint8_t l2, r2;
	int16_t lx, ly, rx, ry;
} yw_controller;

const char *yw_error_string(int err);
const char *yw_quit_reason_string(int code);
bool yw_quit_reason_is_in_use(int code);
bool yw_quit_reason_is_stopped(int code);
// PS5 主动结束串流（关机、待机、用户在 PS5 上退出远程游玩）
bool yw_quit_reason_is_remote_shutdown(int code);

// 创建会话（还没开始连接）。失败返回 NULL，错误写进 err。
yw_session *yw_session_new(const yw_session_params *params, const yw_callbacks *cb, char *err, size_t err_size);
int yw_session_start(yw_session *s);
void yw_session_set_controller(yw_session *s, const yw_controller *c);
void yw_session_set_controller_idle(yw_session *s);
int yw_session_request_idr(yw_session *s);
void yw_session_set_inject_loss(yw_session *s, double loss); // 补丁 0001；< 0 恢复正常上报
int yw_session_goto_bed(yw_session *s);
// 正常结束会话并等待线程退出；可以重复调用
void yw_session_stop(yw_session *s);
void yw_session_free(yw_session *s);

// 发送唤醒包。credential 由 regist_key 的 ASCII 十六进制字符串解析而来。
int yw_wakeup(const char *host, const uint8_t regist_key[16], uint32_t log_mask,
	void (*on_log)(void *user, int level, const char *msg), void *user);

// SIGTERM / SIGINT 只置标志，由主循环轮询后正常结束会话（launchd 停服务时用）
void yw_install_signal_handlers(void);
bool yw_shutdown_requested(void);

#ifdef __cplusplus
}
#endif

#endif
