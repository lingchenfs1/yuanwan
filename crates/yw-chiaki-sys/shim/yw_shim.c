// SPDX-License-Identifier: AGPL-3.0-only
// yw_shim 实现：把 libchiaki 的会话包成不透明句柄。见 yw_shim.h。

#include "yw_shim.h"

#include <chiaki/audioreceiver.h>
#include <chiaki/controller.h>
#include <chiaki/discovery.h>
#include <chiaki/log.h>
#include <chiaki/session.h>

#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

struct yw_session
{
	ChiakiSession session;
	ChiakiLog log;
	ChiakiConnectInfo ci;
	yw_callbacks cb;
	ChiakiAudioSink audio_sink;
	ChiakiAudioSink haptics_sink;
	char host[256];
	uint32_t console_pin;
	bool started;
	bool stopped;
	bool initialized;
};

// ---------------------------------------------------------------- 回调转发

static void log_cb(ChiakiLogLevel level, const char *msg, void *user)
{
	yw_session *s = user;
	if(s->cb.on_log)
		s->cb.on_log(s->cb.user, (int)level, msg);
}

static bool video_cb(uint8_t *buf, size_t buf_size, int32_t frames_lost, bool frame_recovered, void *user)
{
	yw_session *s = user;
	if(s->cb.on_video)
		s->cb.on_video(s->cb.user, buf, buf_size, frames_lost, frame_recovered ? 1 : 0);
	return true;
}

// 必须注册音频接收端：chiaki-ng v1.10.1 在没有接收端时（audioreceiver.c:259）直接 return，会漏掉音频缓冲
static void audio_header_cb(ChiakiAudioHeader *h, void *user)
{
	yw_session *s = user;
	if(s->cb.on_audio_header)
		s->cb.on_audio_header(s->cb.user, h->channels, h->bits, h->rate, h->frame_size);
}

static void audio_frame_cb(uint8_t *buf, size_t buf_size, void *user)
{
	yw_session *s = user;
	if(s->cb.on_audio)
		s->cb.on_audio(s->cb.user, buf, buf_size);
}

static void haptics_header_cb(ChiakiAudioHeader *h, void *user)
{
	(void)h;
	(void)user;
}

static void haptics_frame_cb(uint8_t *buf, size_t buf_size, void *user)
{
	yw_session *s = user;
	if(s->cb.on_haptics)
		s->cb.on_haptics(s->cb.user, buf, buf_size);
}

static void event_cb(ChiakiEvent *event, void *user)
{
	yw_session *s = user;
	switch(event->type)
	{
		case CHIAKI_EVENT_CONNECTED:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_CONNECTED, 0, NULL);
			break;
		case CHIAKI_EVENT_QUIT:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_QUIT, (int)event->quit.reason, event->quit.reason_str);
			break;
		case CHIAKI_EVENT_LOGIN_PIN_REQUEST:
			// 只转发。这个回调在会话线程持有 state_mutex 时调用（session.c:546），
			// 在这里调 chiaki_session_set_login_pin 会在非递归锁上自锁。
			// PIN 已在 yw_session_new 里预先设好；PIN 不对时由上层结束会话。
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_LOGIN_PIN, event->login_pin_request.pin_incorrect ? 1 : 0, NULL);
			break;
		case CHIAKI_EVENT_RUMBLE:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_RUMBLE, (event->rumble.left << 8) | event->rumble.right, NULL);
			break;
		case CHIAKI_EVENT_TRIGGER_EFFECTS:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_TRIGGER_EFFECTS, 0, NULL);
			break;
		case CHIAKI_EVENT_VIDEO_FEC_FAILURE:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_FEC_FAILURE, 0, NULL);
			break;
		default:
			if(s->cb.on_event)
				s->cb.on_event(s->cb.user, YW_EV_OTHER, (int)event->type, NULL);
			break;
	}
}

// ---------------------------------------------------------------- 会话

const char *yw_error_string(int err)
{
	return chiaki_error_string((ChiakiErrorCode)err);
}

const char *yw_quit_reason_string(int code)
{
	return chiaki_quit_reason_string((ChiakiQuitReason)code);
}

bool yw_quit_reason_is_in_use(int code)
{
	return code == CHIAKI_QUIT_REASON_SESSION_REQUEST_RP_IN_USE;
}

bool yw_quit_reason_is_stopped(int code)
{
	return code == CHIAKI_QUIT_REASON_STOPPED;
}

bool yw_quit_reason_is_remote_shutdown(int code)
{
	return code == CHIAKI_QUIT_REASON_STREAM_CONNECTION_REMOTE_SHUTDOWN;
}

yw_session *yw_session_new(const yw_session_params *p, const yw_callbacks *cb, char *err, size_t err_size)
{
	if(!p || !p->host)
	{
		snprintf(err, err_size, "参数缺失");
		return NULL;
	}
	yw_session *s = calloc(1, sizeof(yw_session));
	if(!s)
	{
		snprintf(err, err_size, "内存不足");
		return NULL;
	}
	if(cb)
		s->cb = *cb;
	snprintf(s->host, sizeof(s->host), "%s", p->host);
	s->console_pin = p->console_pin;
	chiaki_log_init(&s->log, p->log_mask ? p->log_mask : (CHIAKI_LOG_ALL & ~CHIAKI_LOG_VERBOSE & ~CHIAKI_LOG_DEBUG), log_cb, s);

	ChiakiVideoResolutionPreset res = CHIAKI_VIDEO_RESOLUTION_PRESET_1080p;
	if(p->height == 720) res = CHIAKI_VIDEO_RESOLUTION_PRESET_720p;
	else if(p->height == 540) res = CHIAKI_VIDEO_RESOLUTION_PRESET_540p;
	else if(p->height == 360) res = CHIAKI_VIDEO_RESOLUTION_PRESET_360p;
	ChiakiVideoFPSPreset fps = p->fps == 30 ? CHIAKI_VIDEO_FPS_PRESET_30 : CHIAKI_VIDEO_FPS_PRESET_60;

	ChiakiConnectInfo *ci = &s->ci;
	ci->ps5 = true;
	ci->host = s->host;
	memcpy(ci->regist_key, p->regist_key, sizeof(ci->regist_key));
	memcpy(ci->morning, p->morning, sizeof(ci->morning));
	chiaki_connect_video_profile_preset(&ci->video_profile, res, fps);
	if(p->bitrate_kbps)
		ci->video_profile.bitrate = p->bitrate_kbps;
	ci->video_profile.codec = p->codec == 0 ? CHIAKI_CODEC_H264 : p->codec == 2 ? CHIAKI_CODEC_H265_HDR : CHIAKI_CODEC_H265;
	ci->video_profile_auto_downgrade = true;
	ci->enable_keyboard = false;
	ci->enable_dualsense = true;
	ci->audio_video_disabled = CHIAKI_NONE_DISABLED;
	ci->auto_regist = false;
	ci->packet_loss_max = p->packet_loss_max > 0 ? p->packet_loss_max : 0.05;
	ci->enable_idr_on_fec_failure = false;

	ChiakiErrorCode e = chiaki_session_init(&s->session, ci, &s->log);
	memset(ci->regist_key, 0, sizeof(ci->regist_key));
	memset(ci->morning, 0, sizeof(ci->morning));
	if(e != CHIAKI_ERR_SUCCESS)
	{
		snprintf(err, err_size, "会话初始化失败：%s", chiaki_error_string(e));
		free(s);
		return NULL;
	}
	s->initialized = true;
	if(s->console_pin > 0)
	{
		// 开始前预设登录 PIN：PS5 要 PIN 时会话线程直接取用，不必在回调里设置
		char pin[8];
		snprintf(pin, sizeof(pin), "%04u", s->console_pin % 10000);
		chiaki_session_set_login_pin(&s->session, (const uint8_t *)pin, 4);
		memset(pin, 0, sizeof(pin));
	}
	chiaki_session_set_event_cb(&s->session, event_cb, s);
	chiaki_session_set_video_sample_cb(&s->session, video_cb, s);
	s->audio_sink = (ChiakiAudioSink){ .user = s, .header_cb = audio_header_cb, .frame_cb = audio_frame_cb };
	chiaki_session_set_audio_sink(&s->session, &s->audio_sink);
	s->haptics_sink = (ChiakiAudioSink){ .user = s, .header_cb = haptics_header_cb, .frame_cb = haptics_frame_cb };
	chiaki_session_set_haptics_sink(&s->session, &s->haptics_sink);
	if(p->ls_mtu || p->ls_rtt_ms)
		chiaki_session_yw_set_launchspec_override(&s->session, p->ls_mtu, p->ls_rtt_ms);
	return s;
}

int yw_session_start(yw_session *s)
{
	if(!s || s->started)
		return CHIAKI_ERR_INVALID_DATA;
	ChiakiErrorCode e = chiaki_session_start(&s->session);
	if(e == CHIAKI_ERR_SUCCESS)
		s->started = true;
	return (int)e;
}

void yw_session_set_controller(yw_session *s, const yw_controller *c)
{
	ChiakiControllerState st;
	chiaki_controller_state_set_idle(&st);
	st.buttons = c->buttons;
	st.l2_state = c->l2;
	st.r2_state = c->r2;
	st.left_x = c->lx;
	st.left_y = c->ly;
	st.right_x = c->rx;
	st.right_y = c->ry;
	chiaki_session_set_controller_state(&s->session, &st);
}

void yw_session_set_controller_idle(yw_session *s)
{
	ChiakiControllerState st;
	chiaki_controller_state_set_idle(&st);
	chiaki_session_set_controller_state(&s->session, &st);
}

int yw_session_request_idr(yw_session *s)
{
	return (int)chiaki_session_request_idr(&s->session);
}

void yw_session_set_inject_loss(yw_session *s, double loss)
{
	chiaki_session_yw_set_inject_loss(&s->session, loss);
}

int yw_session_goto_bed(yw_session *s)
{
	return (int)chiaki_session_goto_bed(&s->session);
}

void yw_session_stop(yw_session *s)
{
	if(!s || !s->started || s->stopped)
		return;
	chiaki_session_stop(&s->session);
	chiaki_session_join(&s->session);
	s->stopped = true;
}

void yw_session_free(yw_session *s)
{
	if(!s)
		return;
	yw_session_stop(s);
	if(s->initialized)
		chiaki_session_fini(&s->session);
	free(s);
}

// ---------------------------------------------------------------- 唤醒

typedef struct
{
	void (*on_log)(void *user, int level, const char *msg);
	void *user;
} WakeLogCtx;

static void wake_log_cb(ChiakiLogLevel level, const char *msg, void *user)
{
	WakeLogCtx *c = user;
	if(c->on_log)
		c->on_log(c->user, (int)level, msg);
}

int yw_wakeup(const char *host, const uint8_t regist_key[16], uint32_t log_mask,
	void (*on_log)(void *user, int level, const char *msg), void *user)
{
	char hex[17];
	memcpy(hex, regist_key, 16);
	hex[16] = '\0';
	uint64_t credential = strtoull(hex, NULL, 16);
	WakeLogCtx ctx = { on_log, user };
	ChiakiLog log;
	chiaki_log_init(&log, log_mask ? log_mask : (CHIAKI_LOG_ERROR | CHIAKI_LOG_WARNING), wake_log_cb, &ctx);
	return (int)chiaki_discovery_wakeup(&log, NULL, host, credential, true);
}

// ---------------------------------------------------------------- 信号

static volatile sig_atomic_t g_shutdown = 0;

static void on_signal(int sig)
{
	(void)sig;
	g_shutdown = 1;
}

void yw_install_signal_handlers(void)
{
	struct sigaction sa;
	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = on_signal;
	sigemptyset(&sa.sa_mask);
	sigaction(SIGTERM, &sa, NULL);
	sigaction(SIGINT, &sa, NULL);
	signal(SIGPIPE, SIG_IGN);
}

bool yw_shutdown_requested(void)
{
	return g_shutdown != 0;
}
