// SPDX-License-Identifier: AGPL-3.0-only
// yw-m0-probe：M0 阶段的 PS5 会话实验工具（链接 chiaki-ng 核心库，所以按 AGPL 发布）。
//
// 子命令：
//   status   <主机>                         单播查询 PS5 状态（开机 / 待机）
//   wakeup   <主机> [--cred 文件]            发送唤醒包
//   register <主机> --pin 8位 --account ID [--cred 文件]
//                                            注册（配对），凭据写入文件，权限 600
//   stream   <主机> [选项]                    建立串流会话，按时间表做实验，写日志
//
// 凭据默认存在 ~/.config/yw/ps5.cred。凭据内容一律不打印、不写日志。

#include <chiaki/session.h>
#include <chiaki/regist.h>
#include <chiaki/discovery.h>
#include <chiaki/controller.h>
#include <chiaki/base64.h>
#include <chiaki/time.h>
#include <chiaki/log.h>

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <getopt.h>
#include <netinet/in.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

// ---------------------------------------------------------------- 日志

static FILE *g_log_file = NULL;
static pthread_mutex_t g_log_mutex = PTHREAD_MUTEX_INITIALIZER;
static uint64_t g_t0_us = 0;
static volatile sig_atomic_t g_interrupted = 0;

static double now_s(void)
{
	return (double)(chiaki_time_now_monotonic_us() - g_t0_us) / 1e6;
}

static char level_char(ChiakiLogLevel level)
{
	switch(level)
	{
		case CHIAKI_LOG_ERROR: return 'E';
		case CHIAKI_LOG_WARNING: return 'W';
		case CHIAKI_LOG_INFO: return 'I';
		case CHIAKI_LOG_VERBOSE: return 'V';
		default: return 'D';
	}
}

// VERBOSE/DEBUG 日志很多，只留实验需要的几类
static bool keep_verbose(const char *msg)
{
	static const char *keys[] = {
		"[yw-", "connection quality", "measured bitrate", "Congestion Control Packet",
		"IDR", "Senkusha", "MTU", "RTT", "Corrupt", "corrupt", "Switched to profile",
		"reorder timeout", "dropping AV packet", NULL // Takion 重排队列的跳过和丢弃（定位收包端丢包）
	};
	for(int i = 0; keys[i]; i++)
		if(strstr(msg, keys[i]))
			return true;
	return false;
}

static void ylog(char level, const char *fmt, ...)
{
	char buf[1024];
	va_list ap;
	va_start(ap, fmt);
	vsnprintf(buf, sizeof(buf), fmt, ap);
	va_end(ap);
	double t = now_s();
	pthread_mutex_lock(&g_log_mutex);
	if(g_log_file)
	{
		fprintf(g_log_file, "%.3f %c %s\n", t, level, buf);
		fflush(g_log_file);
	}
	if(level != 'V' && level != 'D')
		fprintf(stderr, "%.3f %c %s\n", t, level, buf);
	pthread_mutex_unlock(&g_log_mutex);
}

static void chiaki_log_cb(ChiakiLogLevel level, const char *msg, void *user)
{
	(void)user;
	if((level == CHIAKI_LOG_VERBOSE || level == CHIAKI_LOG_DEBUG) && !keep_verbose(msg))
		return;
	double t = now_s();
	pthread_mutex_lock(&g_log_mutex);
	if(g_log_file)
	{
		fprintf(g_log_file, "%.3f %c %s\n", t, level_char(level), msg);
		fflush(g_log_file);
	}
	// 终端只显示 INFO 及以上，而且不刷每帧统计
	if(level != CHIAKI_LOG_VERBOSE && level != CHIAKI_LOG_DEBUG)
		fprintf(stderr, "%.3f %c %s\n", t, level_char(level), msg);
	pthread_mutex_unlock(&g_log_mutex);
}

static void on_sigint(int sig)
{
	(void)sig;
	g_interrupted = 1;
}

// ---------------------------------------------------------------- 凭据文件

typedef struct
{
	uint8_t regist_key[CHIAKI_SESSION_AUTH_SIZE]; // 原样保存 16 字节（内容是 ASCII 十六进制字符加 \0 填充）
	uint8_t rp_key[0x10];                         // morning
	uint32_t rp_key_type;
	uint32_t console_pin;
	uint8_t server_mac[6];
	char nickname[0x21];
	int target;
} Cred;

static void default_cred_path(char *out, size_t out_size)
{
	const char *home = getenv("HOME");
	snprintf(out, out_size, "%s/.config/yw/ps5.cred", home ? home : ".");
}

static void hex_encode(const uint8_t *in, size_t n, char *out)
{
	static const char *digits = "0123456789abcdef";
	for(size_t i = 0; i < n; i++)
	{
		out[i * 2] = digits[in[i] >> 4];
		out[i * 2 + 1] = digits[in[i] & 0xf];
	}
	out[n * 2] = '\0';
}

static bool hex_decode(const char *in, uint8_t *out, size_t n)
{
	if(strlen(in) != n * 2)
		return false;
	for(size_t i = 0; i < n; i++)
	{
		unsigned int v;
		if(sscanf(in + i * 2, "%2x", &v) != 1)
			return false;
		out[i] = (uint8_t)v;
	}
	return true;
}

static bool mkdir_p_for(const char *path)
{
	char dir[1024];
	snprintf(dir, sizeof(dir), "%s", path);
	char *slash = strrchr(dir, '/');
	if(!slash)
		return true;
	*slash = '\0';
	for(char *p = dir + 1; *p; p++)
	{
		if(*p == '/')
		{
			*p = '\0';
			mkdir(dir, 0700);
			*p = '/';
		}
	}
	return mkdir(dir, 0700) == 0 || errno == EEXIST;
}

static bool cred_save(const char *path, const Cred *c)
{
	if(!mkdir_p_for(path))
		return false;
	char tmp[1100];
	snprintf(tmp, sizeof(tmp), "%s.tmp", path);
	int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
	if(fd < 0)
		return false;
	FILE *f = fdopen(fd, "w");
	if(!f)
	{
		close(fd);
		return false;
	}
	char regist_hex[CHIAKI_SESSION_AUTH_SIZE * 2 + 1], rp_hex[0x10 * 2 + 1], mac_hex[13];
	hex_encode(c->regist_key, sizeof(c->regist_key), regist_hex);
	hex_encode(c->rp_key, sizeof(c->rp_key), rp_hex);
	hex_encode(c->server_mac, sizeof(c->server_mac), mac_hex);
	fprintf(f, "# yw PS5 凭据（由 yw-m0-probe register 生成，勿外传、勿提交）\n");
	fprintf(f, "target=%d\n", c->target);
	fprintf(f, "server_mac=%s\n", mac_hex);
	fprintf(f, "nickname=%s\n", c->nickname);
	fprintf(f, "rp_key_type=%u\n", c->rp_key_type);
	fprintf(f, "console_pin=%u\n", c->console_pin);
	fprintf(f, "regist_key=%s\n", regist_hex);
	fprintf(f, "rp_key=%s\n", rp_hex);
	bool ok = fclose(f) == 0;
	if(ok)
		ok = rename(tmp, path) == 0;
	return ok;
}

static bool cred_load(const char *path, Cred *c)
{
	memset(c, 0, sizeof(*c));
	FILE *f = fopen(path, "r");
	if(!f)
		return false;
	char line[512];
	bool have_regist = false, have_rp = false;
	while(fgets(line, sizeof(line), f))
	{
		line[strcspn(line, "\r\n")] = '\0';
		if(line[0] == '#' || !strchr(line, '='))
			continue;
		char *eq = strchr(line, '=');
		*eq = '\0';
		const char *k = line, *v = eq + 1;
		if(!strcmp(k, "target")) c->target = atoi(v);
		else if(!strcmp(k, "server_mac")) hex_decode(v, c->server_mac, sizeof(c->server_mac));
		else if(!strcmp(k, "nickname")) snprintf(c->nickname, sizeof(c->nickname), "%s", v);
		else if(!strcmp(k, "rp_key_type")) c->rp_key_type = (uint32_t)strtoul(v, NULL, 10);
		else if(!strcmp(k, "console_pin")) c->console_pin = (uint32_t)strtoul(v, NULL, 10);
		else if(!strcmp(k, "regist_key")) have_regist = hex_decode(v, c->regist_key, sizeof(c->regist_key));
		else if(!strcmp(k, "rp_key")) have_rp = hex_decode(v, c->rp_key, sizeof(c->rp_key));
	}
	fclose(f);
	return have_regist && have_rp;
}

// 唤醒用的凭据：regist key 的 ASCII 十六进制字符串按 16 进制解析成整数
static uint64_t cred_wakeup_credential(const Cred *c)
{
	char s[CHIAKI_SESSION_AUTH_SIZE + 1];
	memcpy(s, c->regist_key, CHIAKI_SESSION_AUTH_SIZE);
	s[CHIAKI_SESSION_AUTH_SIZE] = '\0';
	return strtoull(s, NULL, 16);
}

// ---------------------------------------------------------------- status

static int cmd_status(const char *host)
{
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	if(fd < 0)
	{
		perror("socket");
		return 1;
	}
	struct sockaddr_in local = { .sin_family = AF_INET, .sin_addr.s_addr = htonl(INADDR_ANY) };
	for(int port = 9303; port <= 9319; port++)
	{
		local.sin_port = htons((uint16_t)port);
		if(bind(fd, (struct sockaddr *)&local, sizeof(local)) == 0)
			break;
	}
	struct sockaddr_in dst = { .sin_family = AF_INET, .sin_port = htons(9302) };
	if(inet_pton(AF_INET, host, &dst.sin_addr) != 1)
	{
		fprintf(stderr, "主机地址必须是 IPv4：%s\n", host);
		close(fd);
		return 1;
	}
	struct timeval tv = { .tv_sec = 2 };
	setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
	const char *msg = "SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n";
	for(int i = 0; i < 3; i++)
	{
		uint64_t t0 = chiaki_time_now_monotonic_us();
		sendto(fd, msg, strlen(msg), 0, (struct sockaddr *)&dst, sizeof(dst));
		char buf[2048];
		ssize_t n = recv(fd, buf, sizeof(buf) - 1, 0);
		double ms = (double)(chiaki_time_now_monotonic_us() - t0) / 1000.0;
		if(n <= 0)
		{
			printf("#%d 超时\n", i);
			continue;
		}
		buf[n] = '\0';
		const char *state = strstr(buf, " 200 ") ? "开机（就绪）" : strstr(buf, " 620 ") ? "待机" : "未知";
		char *ver = strstr(buf, "system-version:");
		char version[32] = "?";
		if(ver)
			sscanf(ver, "system-version:%31s", version);
		printf("#%d %.1f ms：%s，系统版本 %s\n", i, ms, state, version);
		usleep(300 * 1000);
	}
	close(fd);
	return 0;
}

// ---------------------------------------------------------------- wakeup

static int cmd_wakeup(const char *host, const char *cred_path)
{
	Cred c;
	if(!cred_load(cred_path, &c))
	{
		fprintf(stderr, "读不到凭据：%s（先 register）\n", cred_path);
		return 1;
	}
	ChiakiLog log;
	chiaki_log_init(&log, CHIAKI_LOG_ALL & ~CHIAKI_LOG_VERBOSE & ~CHIAKI_LOG_DEBUG, chiaki_log_cb, NULL);
	ChiakiErrorCode err = chiaki_discovery_wakeup(&log, NULL, host, cred_wakeup_credential(&c), true);
	if(err != CHIAKI_ERR_SUCCESS)
	{
		fprintf(stderr, "唤醒包发送失败：%s\n", chiaki_error_string(err));
		return 1;
	}
	printf("唤醒包已发送到 %s。之后用 status 轮询，直到显示「开机（就绪）」。\n", host);
	return 0;
}

// ---------------------------------------------------------------- register

typedef struct
{
	pthread_mutex_t mutex;
	pthread_cond_t cond;
	bool done;
	bool success;
	ChiakiRegisteredHost host;
} RegistCtx;

static void regist_cb(ChiakiRegistEvent *event, void *user)
{
	RegistCtx *ctx = user;
	pthread_mutex_lock(&ctx->mutex);
	ctx->done = true;
	ctx->success = event->type == CHIAKI_REGIST_EVENT_TYPE_FINISHED_SUCCESS;
	if(ctx->success && event->registered_host)
		ctx->host = *event->registered_host;
	pthread_cond_signal(&ctx->cond);
	pthread_mutex_unlock(&ctx->mutex);
}

// 账号 ID 可以是十进制数字，也可以是 8 字节小端的 base64
static bool parse_account_id(const char *s, uint8_t out[CHIAKI_PSN_ACCOUNT_ID_SIZE])
{
	bool all_digits = *s != '\0';
	for(const char *p = s; *p; p++)
		if(*p < '0' || *p > '9')
			all_digits = false;
	if(all_digits)
	{
		errno = 0;
		unsigned long long v = strtoull(s, NULL, 10);
		if(errno)
			return false;
		for(int i = 0; i < CHIAKI_PSN_ACCOUNT_ID_SIZE; i++)
			out[i] = (uint8_t)(v >> (8 * i));
		return true;
	}
	size_t out_size = CHIAKI_PSN_ACCOUNT_ID_SIZE;
	ChiakiErrorCode err = chiaki_base64_decode(s, strlen(s), out, &out_size);
	return err == CHIAKI_ERR_SUCCESS && out_size == CHIAKI_PSN_ACCOUNT_ID_SIZE;
}

static int cmd_register(const char *host, const char *pin_str, const char *account, const char *cred_path, const char *log_path)
{
	if(!pin_str || strlen(pin_str) != 8 || strspn(pin_str, "0123456789") != 8)
	{
		fprintf(stderr, "--pin 必须是 PS5 上显示的 8 位数字\n");
		return 1;
	}
	ChiakiRegistInfo info;
	memset(&info, 0, sizeof(info));
	if(!account || !parse_account_id(account, info.psn_account_id))
	{
		fprintf(stderr, "--account 必须是十进制账号 ID，或 8 字节的 base64\n");
		return 1;
	}
	if(log_path)
		g_log_file = fopen(log_path, "a");
	info.target = CHIAKI_TARGET_PS5_1;
	info.host = host;
	info.broadcast = false;
	info.psn_online_id = NULL;
	info.pin = (uint32_t)strtoul(pin_str, NULL, 10);
	info.console_pin = 0;
	info.holepunch_info = NULL;
	info.rudp = NULL;

	ChiakiLog log;
	chiaki_log_init(&log, CHIAKI_LOG_ALL & ~CHIAKI_LOG_DEBUG, chiaki_log_cb, NULL);
	RegistCtx ctx;
	memset(&ctx, 0, sizeof(ctx));
	pthread_mutex_init(&ctx.mutex, NULL);
	pthread_cond_init(&ctx.cond, NULL);

	ChiakiRegist regist;
	ChiakiErrorCode err = chiaki_regist_start(&regist, &log, &info, regist_cb, &ctx);
	memset(info.psn_account_id, 0, sizeof(info.psn_account_id));
	if(err != CHIAKI_ERR_SUCCESS)
	{
		fprintf(stderr, "注册启动失败：%s\n", chiaki_error_string(err));
		return 1;
	}
	pthread_mutex_lock(&ctx.mutex);
	struct timespec deadline;
	clock_gettime(CLOCK_REALTIME, &deadline);
	deadline.tv_sec += 30;
	while(!ctx.done)
		if(pthread_cond_timedwait(&ctx.cond, &ctx.mutex, &deadline) == ETIMEDOUT)
			break;
	bool done = ctx.done, success = ctx.success;
	pthread_mutex_unlock(&ctx.mutex);
	if(!done)
		chiaki_regist_stop(&regist);
	chiaki_regist_fini(&regist);
	if(!done || !success)
	{
		fprintf(stderr, "注册失败（%s）。确认 PS5 正停在「设置 > 系统 > 遥控游玩 > 连接设备」的配对码界面，且 PIN 没过期。\n",
			done ? "PS5 拒绝或出错" : "30 秒超时");
		return 1;
	}

	Cred c;
	memset(&c, 0, sizeof(c));
	memcpy(c.regist_key, ctx.host.rp_regist_key, sizeof(c.regist_key));
	memcpy(c.rp_key, ctx.host.rp_key, sizeof(c.rp_key));
	c.rp_key_type = ctx.host.rp_key_type;
	c.console_pin = ctx.host.console_pin;
	memcpy(c.server_mac, ctx.host.server_mac, sizeof(c.server_mac));
	snprintf(c.nickname, sizeof(c.nickname), "%.*s", (int)sizeof(ctx.host.server_nickname), ctx.host.server_nickname);
	c.target = (int)ctx.host.target;
	memset(&ctx.host, 0, sizeof(ctx.host));
	if(!cred_save(cred_path, &c))
	{
		fprintf(stderr, "凭据写入失败：%s\n", cred_path);
		return 1;
	}
	printf("注册成功：主机「%s」，凭据已写入 %s（权限 600）\n", c.nickname, cred_path);
	return 0;
}

// ---------------------------------------------------------------- stream

typedef struct
{
	double t;
	double value;
} PlanStep;

#define PLAN_MAX 64

typedef struct
{
	ChiakiSession session;
	FILE *dump;
	bool hevc;
	volatile bool connected;
	volatile bool quit;
	uint32_t console_pin;
	// 统计（视频回调线程写，主线程读，只用于粗略显示）
	volatile uint64_t frames;
	volatile uint64_t bytes;
	volatile uint64_t keyframes;
	volatile int64_t frames_lost;
	volatile uint64_t frames_recovered;
	volatile uint64_t audio_frames;
	volatile uint64_t haptic_frames;
	// 截屏：收到 snap 命令后请求 IDR，从下一个关键帧起连同参数集写 SNAP_FRAMES 帧
	pthread_mutex_t snap_mutex;
	uint8_t *header;          // 最近一次档位切换时下发的参数集（VPS/SPS/PPS）
	size_t header_size;
	int snap_state;           // 0 空闲，1 等关键帧，2 写入中
	FILE *snap_file;
	int snap_frames;
	char snap_path[512];
	double connect_t;
	double first_frame_t;
} StreamCtx;

// 找出访问单元里最「重要」的 NAL 类型：有 IDR 就算关键帧
static bool sample_is_keyframe(const uint8_t *buf, size_t size, bool hevc)
{
	for(size_t i = 0; i + 3 < size; i++)
	{
		if(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)
		{
			uint8_t b = buf[i + 3];
			if(hevc)
			{
				int type = (b >> 1) & 0x3f;
				if(type == 19 || type == 20)
					return true;
			}
			else
			{
				if((b & 0x1f) == 5)
					return true;
			}
			i += 3;
		}
	}
	return false;
}

// 只含参数集、没有图像片的样本（chiaki 在档位切换时单独回调一次参数集）
static bool sample_is_param_sets_only(const uint8_t *buf, size_t size, bool hevc)
{
	bool params = false;
	for(size_t i = 0; i + 3 < size; i++)
	{
		if(buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1)
		{
			uint8_t b = buf[i + 3];
			int type = hevc ? (b >> 1) & 0x3f : b & 0x1f;
			bool is_param = hevc ? (type >= 32 && type <= 34) : (type == 7 || type == 8);
			bool is_slice = hevc ? type <= 31 : (type >= 1 && type <= 5);
			if(is_slice)
				return false;
			if(is_param)
				params = true;
			i += 3;
		}
	}
	return params;
}

#define SNAP_FRAMES 8

static void snap_feed(StreamCtx *ctx, const uint8_t *buf, size_t buf_size, bool key)
{
	pthread_mutex_lock(&ctx->snap_mutex);
	if(ctx->snap_state == 1 && key)
	{
		char tmp[600];
		snprintf(tmp, sizeof(tmp), "%s.tmp", ctx->snap_path);
		ctx->snap_file = fopen(tmp, "wb");
		if(ctx->snap_file)
		{
			if(ctx->header)
				fwrite(ctx->header, 1, ctx->header_size, ctx->snap_file);
			fwrite(buf, 1, buf_size, ctx->snap_file);
			ctx->snap_frames = 1;
			ctx->snap_state = 2;
		}
		else
			ctx->snap_state = 0;
	}
	else if(ctx->snap_state == 2)
	{
		fwrite(buf, 1, buf_size, ctx->snap_file);
		if(++ctx->snap_frames >= SNAP_FRAMES)
		{
			fclose(ctx->snap_file);
			ctx->snap_file = NULL;
			char tmp[600];
			snprintf(tmp, sizeof(tmp), "%s.tmp", ctx->snap_path);
			rename(tmp, ctx->snap_path);
			ctx->snap_state = 0;
			ylog('I', "[yw-ctl] 截屏码流已写入 %s", ctx->snap_path);
		}
	}
	pthread_mutex_unlock(&ctx->snap_mutex);
}

static bool video_cb(uint8_t *buf, size_t buf_size, int32_t frames_lost, bool frame_recovered, void *user)
{
	StreamCtx *ctx = user;
	if(ctx->dump)
		fwrite(buf, 1, buf_size, ctx->dump);
	if(sample_is_param_sets_only(buf, buf_size, ctx->hevc))
	{
		pthread_mutex_lock(&ctx->snap_mutex);
		free(ctx->header);
		ctx->header = malloc(buf_size);
		memcpy(ctx->header, buf, buf_size);
		ctx->header_size = buf_size;
		pthread_mutex_unlock(&ctx->snap_mutex);
		ylog('V', "[yw-sample] 参数集 size=%zu", buf_size);
		return true;
	}
	bool key = sample_is_keyframe(buf, buf_size, ctx->hevc);
	snap_feed(ctx, buf, buf_size, key);
	if(ctx->frames == 0)
		ctx->first_frame_t = now_s();
	ctx->frames++;
	ctx->bytes += buf_size;
	if(key)
		ctx->keyframes++;
	ctx->frames_lost += frames_lost;
	if(frame_recovered)
		ctx->frames_recovered++;
	ylog('V', "[yw-sample] n=%llu size=%zu key=%d lost=%d recovered=%d",
		(unsigned long long)ctx->frames, buf_size, key ? 1 : 0, (int)frames_lost, frame_recovered ? 1 : 0);
	return true;
}

// 音频和触觉只计数、不解码。注意：必须注册接收端——chiaki-ng v1.10.1 在没有接收端时
// （audioreceiver.c 第 259 行）直接 return，会漏掉已分配的音频缓冲。
static void audio_header_cb(ChiakiAudioHeader *header, void *user)
{
	(void)user;
	ylog('I', "[yw-audio] 声道 %u，位深 %u，采样率 %u，帧长 %u", header->channels, header->bits, header->rate, header->frame_size);
}

static void audio_frame_cb(uint8_t *buf, size_t buf_size, void *user)
{
	(void)buf;
	(void)buf_size;
	StreamCtx *ctx = user;
	ctx->audio_frames++;
}

static void haptics_header_cb(ChiakiAudioHeader *header, void *user)
{
	(void)header;
	(void)user;
}

static void haptics_frame_cb(uint8_t *buf, size_t buf_size, void *user)
{
	(void)buf;
	(void)buf_size;
	StreamCtx *ctx = user;
	ctx->haptic_frames++;
}

static void event_cb(ChiakiEvent *event, void *user)
{
	StreamCtx *ctx = user;
	switch(event->type)
	{
		case CHIAKI_EVENT_CONNECTED:
			ctx->connected = true;
			ctx->connect_t = now_s();
			ylog('I', "[yw-event] 已连接");
			break;
		case CHIAKI_EVENT_LOGIN_PIN_REQUEST:
			// 回调时会话线程持有 state_mutex，这里不能调 chiaki_session_set_login_pin（会自锁）；
			// PIN 在 chiaki_session_start 之前已预设。
			if(ctx->console_pin > 0 && !event->login_pin_request.pin_incorrect)
				ylog('I', "[yw-event] PS5 要求登录 PIN，使用预设的 PIN");
			else
			{
				ylog('E', "[yw-event] PS5 要求登录 PIN（%s），用 --console-pin 提供",
					event->login_pin_request.pin_incorrect ? "上次的不对" : "未提供");
				ctx->quit = true;
			}
			break;
		case CHIAKI_EVENT_QUIT:
			ylog('I', "[yw-event] 会话结束：%s%s%s", chiaki_quit_reason_string(event->quit.reason),
				event->quit.reason_str ? " / " : "", event->quit.reason_str ? event->quit.reason_str : "");
			ctx->quit = true;
			break;
		case CHIAKI_EVENT_NICKNAME_RECEIVED:
			ylog('I', "[yw-event] 收到主机名");
			break;
		case CHIAKI_EVENT_VIDEO_FEC_FAILURE:
			ylog('W', "[yw-event] 视频 FEC 失败");
			break;
		case CHIAKI_EVENT_RUMBLE:
			ylog('V', "[yw-event] 震动 left=%u right=%u", event->rumble.left, event->rumble.right);
			break;
		case CHIAKI_EVENT_TRIGGER_EFFECTS:
			ylog('V', "[yw-event] 扳机效果");
			break;
		default:
			ylog('V', "[yw-event] 事件 %d", (int)event->type);
			break;
	}
}

static int parse_plan(const char *s, PlanStep *steps, bool with_value)
{
	int n = 0;
	if(!s)
		return 0;
	char *copy = strdup(s);
	for(char *tok = strtok(copy, ","); tok && n < PLAN_MAX; tok = strtok(NULL, ","))
	{
		PlanStep st = { 0 };
		if(with_value)
		{
			if(sscanf(tok, "%lf:%lf", &st.t, &st.value) != 2)
			{
				fprintf(stderr, "时间表格式错误：%s（应为 秒:值）\n", tok);
				free(copy);
				return -1;
			}
		}
		else if(sscanf(tok, "%lf", &st.t) != 1)
		{
			fprintf(stderr, "时间表格式错误：%s\n", tok);
			free(copy);
			return -1;
		}
		steps[n++] = st;
	}
	free(copy);
	return n;
}

// ---------------------------------------------------------------- 控制管道
// 会话常驻时，从命名管道逐行读命令：
//   press 键[+键...] [按住毫秒]   键：cross circle square triangle up down left right
//                                  l1 r1 l2 r2 l3 r3 options create touchpad ps
//   stick / rstick X Y 毫秒        左 / 右摇杆，-32768~32767
//   snap 文件                      请求 IDR，把下一个关键帧起的 8 帧（含参数集）写入文件
//   inject 丢包率                  注入丢包上报（0~1，-1 恢复正常）
//   idr                            请求 IDR
//   mark 文字                      在日志里打标记
//   quit                           结束会话

static uint32_t button_bits(const char *name, ChiakiControllerState *s)
{
	static const struct { const char *name; uint32_t bit; } map[] = {
		{ "cross", CHIAKI_CONTROLLER_BUTTON_CROSS }, { "circle", CHIAKI_CONTROLLER_BUTTON_MOON },
		{ "square", CHIAKI_CONTROLLER_BUTTON_BOX }, { "triangle", CHIAKI_CONTROLLER_BUTTON_PYRAMID },
		{ "left", CHIAKI_CONTROLLER_BUTTON_DPAD_LEFT }, { "right", CHIAKI_CONTROLLER_BUTTON_DPAD_RIGHT },
		{ "up", CHIAKI_CONTROLLER_BUTTON_DPAD_UP }, { "down", CHIAKI_CONTROLLER_BUTTON_DPAD_DOWN },
		{ "l1", CHIAKI_CONTROLLER_BUTTON_L1 }, { "r1", CHIAKI_CONTROLLER_BUTTON_R1 },
		{ "l3", CHIAKI_CONTROLLER_BUTTON_L3 }, { "r3", CHIAKI_CONTROLLER_BUTTON_R3 },
		{ "options", CHIAKI_CONTROLLER_BUTTON_OPTIONS }, { "create", CHIAKI_CONTROLLER_BUTTON_SHARE },
		{ "touchpad", CHIAKI_CONTROLLER_BUTTON_TOUCHPAD }, { "ps", CHIAKI_CONTROLLER_BUTTON_PS },
		{ NULL, 0 }
	};
	if(!strcmp(name, "l2"))
	{
		s->l2_state = 0xff;
		return CHIAKI_CONTROLLER_ANALOG_BUTTON_L2;
	}
	if(!strcmp(name, "r2"))
	{
		s->r2_state = 0xff;
		return CHIAKI_CONTROLLER_ANALOG_BUTTON_R2;
	}
	for(int i = 0; map[i].name; i++)
		if(!strcmp(name, map[i].name))
			return map[i].bit;
	return 0;
}

// 处理一行命令；返回 false 表示要结束会话
static bool ctl_handle(StreamCtx *ctx, char *line, double rel, ChiakiControllerState *idle)
{
	char *cmd = strtok(line, " \t");
	if(!cmd)
		return true;
	if(!strcmp(cmd, "quit"))
	{
		ylog('I', "[yw-ctl] t=%.1f 收到 quit", rel);
		return false;
	}
	if(!strcmp(cmd, "press"))
	{
		char *keys = strtok(NULL, " \t");
		char *ms_s = strtok(NULL, " \t");
		int ms = ms_s ? atoi(ms_s) : 150;
		if(ms < 30) ms = 30;
		if(ms > 3000) ms = 3000;
		ChiakiControllerState s = *idle;
		bool ok = keys != NULL;
		if(keys)
		{
			char kb[128];
			snprintf(kb, sizeof(kb), "%s", keys);
			for(char *save = NULL, *k = strtok_r(kb, "+", &save); k; k = strtok_r(NULL, "+", &save))
			{
				uint32_t b = button_bits(k, &s);
				if(!b)
					ok = false;
				s.buttons |= b;
			}
		}
		if(!ok)
		{
			ylog('W', "[yw-ctl] 不认识的按键：%s", keys ? keys : "（空）");
			return true;
		}
		chiaki_session_set_controller_state(&ctx->session, &s);
		usleep((useconds_t)ms * 1000);
		chiaki_session_set_controller_state(&ctx->session, idle);
		ylog('I', "[yw-ctl] t=%.1f 按键 %s %d ms", rel, keys, ms);
		return true;
	}
	if(!strcmp(cmd, "stick") || !strcmp(cmd, "rstick"))
	{
		bool right = cmd[0] == 'r';
		char *xs = strtok(NULL, " \t"), *ys = strtok(NULL, " \t"), *ms_s = strtok(NULL, " \t");
		if(!xs || !ys)
			return true;
		int ms = ms_s ? atoi(ms_s) : 200;
		if(ms > 3000) ms = 3000;
		ChiakiControllerState s = *idle;
		if(right)
		{
			s.right_x = (int16_t)atoi(xs);
			s.right_y = (int16_t)atoi(ys);
		}
		else
		{
			s.left_x = (int16_t)atoi(xs);
			s.left_y = (int16_t)atoi(ys);
		}
		chiaki_session_set_controller_state(&ctx->session, &s);
		usleep((useconds_t)ms * 1000);
		chiaki_session_set_controller_state(&ctx->session, idle);
		ylog('V', "[yw-ctl] t=%.1f %s摇杆 %s,%s %d ms", rel, right ? "右" : "左", xs, ys, ms);
		return true;
	}
	if(!strcmp(cmd, "snap"))
	{
		char *path = strtok(NULL, " \t");
		if(!path)
			return true;
		pthread_mutex_lock(&ctx->snap_mutex);
		if(ctx->snap_file)
		{
			fclose(ctx->snap_file);
			ctx->snap_file = NULL;
		}
		snprintf(ctx->snap_path, sizeof(ctx->snap_path), "%s", path);
		ctx->snap_state = 1;
		pthread_mutex_unlock(&ctx->snap_mutex);
		ChiakiErrorCode e = chiaki_session_request_idr(&ctx->session);
		ylog('I', "[yw-plan] t=%.1f 请求 IDR：%s（截屏 %s）", rel, chiaki_error_string(e), path);
		return true;
	}
	if(!strcmp(cmd, "inject"))
	{
		char *v = strtok(NULL, " \t");
		if(!v)
			return true;
		double loss = atof(v);
		chiaki_session_yw_set_inject_loss(&ctx->session, loss);
		ylog('I', "[yw-plan] t=%.1f 注入丢包上报 %s%.3f", rel, loss < 0 ? "关闭 " : "", loss);
		return true;
	}
	if(!strcmp(cmd, "idr"))
	{
		ChiakiErrorCode e = chiaki_session_request_idr(&ctx->session);
		ylog('I', "[yw-plan] t=%.1f 请求 IDR：%s（已收关键帧 %llu）", rel, chiaki_error_string(e), (unsigned long long)ctx->keyframes);
		return true;
	}
	if(!strcmp(cmd, "mark"))
	{
		char *rest = strtok(NULL, "");
		ylog('I', "[yw-mark] t=%.1f %s", rel, rest ? rest : "");
		return true;
	}
	ylog('W', "[yw-ctl] 不认识的命令：%s", cmd);
	return true;
}

typedef struct
{
	const char *ctl_path;
	const char *host;
	const char *cred_path;
	const char *log_path;
	const char *dump_path;
	const char *loss_plan;
	const char *idr_plan;
	int res;
	int fps;
	int bitrate;
	const char *codec;
	double duration;
	double loss_max;
	double wiggle;
	uint32_t console_pin;
	uint32_t ls_mtu;
	uint32_t ls_rtt;
} StreamOpts;

static int cmd_stream(const StreamOpts *o)
{
	Cred c;
	if(!cred_load(o->cred_path, &c))
	{
		fprintf(stderr, "读不到凭据：%s（先 register）\n", o->cred_path);
		return 1;
	}
	PlanStep loss_steps[PLAN_MAX], idr_steps[PLAN_MAX];
	int loss_n = parse_plan(o->loss_plan, loss_steps, true);
	int idr_n = parse_plan(o->idr_plan, idr_steps, false);
	if(loss_n < 0 || idr_n < 0)
		return 1;

	char default_log[256];
	const char *log_path = o->log_path;
	if(!log_path)
	{
		time_t now = time(NULL);
		struct tm tm;
		localtime_r(&now, &tm);
		strftime(default_log, sizeof(default_log), "yw-m0-%Y%m%d-%H%M%S.log", &tm);
		log_path = default_log;
	}
	g_log_file = fopen(log_path, "w");
	if(!g_log_file)
	{
		fprintf(stderr, "日志文件打不开：%s\n", log_path);
		return 1;
	}

	ChiakiVideoResolutionPreset res = CHIAKI_VIDEO_RESOLUTION_PRESET_1080p;
	if(o->res == 720) res = CHIAKI_VIDEO_RESOLUTION_PRESET_720p;
	else if(o->res == 540) res = CHIAKI_VIDEO_RESOLUTION_PRESET_540p;
	else if(o->res == 360) res = CHIAKI_VIDEO_RESOLUTION_PRESET_360p;
	ChiakiVideoFPSPreset fps = o->fps == 30 ? CHIAKI_VIDEO_FPS_PRESET_30 : CHIAKI_VIDEO_FPS_PRESET_60;

	StreamCtx *ctx = calloc(1, sizeof(StreamCtx));
	pthread_mutex_init(&ctx->snap_mutex, NULL);
	ctx->console_pin = o->console_pin ? o->console_pin : c.console_pin;

	int ctl_fd = -1, ctl_keep = -1;
	if(o->ctl_path)
	{
		if(mkfifo(o->ctl_path, 0600) != 0 && errno != EEXIST)
		{
			fprintf(stderr, "建立控制管道失败：%s\n", o->ctl_path);
			return 1;
		}
		ctl_fd = open(o->ctl_path, O_RDONLY | O_NONBLOCK);
		ctl_keep = open(o->ctl_path, O_WRONLY | O_NONBLOCK); // 自己留一个写端，免得写命令的一方关闭后读到 EOF
		if(ctl_fd < 0)
		{
			fprintf(stderr, "打开控制管道失败：%s\n", o->ctl_path);
			return 1;
		}
	}
	char ctl_buf[4096];
	size_t ctl_len = 0;
	bool ctl_quit = false;

	ChiakiConnectInfo ci;
	memset(&ci, 0, sizeof(ci));
	ci.ps5 = true;
	ci.host = o->host;
	memcpy(ci.regist_key, c.regist_key, sizeof(ci.regist_key));
	memcpy(ci.morning, c.rp_key, sizeof(ci.morning));
	chiaki_connect_video_profile_preset(&ci.video_profile, res, fps);
	if(o->bitrate > 0)
		ci.video_profile.bitrate = (unsigned int)o->bitrate;
	ci.video_profile.codec = CHIAKI_CODEC_H265;
	if(o->codec && !strcmp(o->codec, "h264")) ci.video_profile.codec = CHIAKI_CODEC_H264;
	else if(o->codec && !strcmp(o->codec, "hdr")) ci.video_profile.codec = CHIAKI_CODEC_H265_HDR;
	ctx->hevc = ci.video_profile.codec != CHIAKI_CODEC_H264;
	ci.video_profile_auto_downgrade = true;
	ci.enable_keyboard = false;
	ci.enable_dualsense = true;
	ci.audio_video_disabled = CHIAKI_NONE_DISABLED;
	ci.auto_regist = false;
	ci.packet_loss_max = o->loss_max;
	ci.enable_idr_on_fec_failure = false;
	memset(&c, 0, sizeof(c));

	if(o->dump_path)
	{
		ctx->dump = fopen(o->dump_path, "wb");
		if(!ctx->dump)
			ylog('W', "码流导出文件打不开：%s", o->dump_path);
	}

	ylog('I', "[yw-run] 主机 %s，%ux%u@%u，码率 %u kbps，编码 %s，时长 %.0f s，丢包上报上限 %.3f",
		o->host, ci.video_profile.width, ci.video_profile.height, ci.video_profile.max_fps,
		ci.video_profile.bitrate, o->codec ? o->codec : "h265", o->duration, o->loss_max);

	ChiakiLog log;
	chiaki_log_init(&log, CHIAKI_LOG_ALL, chiaki_log_cb, NULL);
	ChiakiErrorCode err = chiaki_session_init(&ctx->session, &ci, &log);
	memset(ci.regist_key, 0, sizeof(ci.regist_key));
	memset(ci.morning, 0, sizeof(ci.morning));
	if(err != CHIAKI_ERR_SUCCESS)
	{
		ylog('E', "会话初始化失败：%s", chiaki_error_string(err));
		return 1;
	}
	if(ctx->console_pin > 0)
	{
		char pin[8];
		snprintf(pin, sizeof(pin), "%04u", ctx->console_pin % 10000);
		chiaki_session_set_login_pin(&ctx->session, (const uint8_t *)pin, 4);
		memset(pin, 0, sizeof(pin));
	}
	chiaki_session_set_event_cb(&ctx->session, event_cb, ctx);
	if(o->ls_mtu || o->ls_rtt)
		chiaki_session_yw_set_launchspec_override(&ctx->session, o->ls_mtu, o->ls_rtt);
	chiaki_session_set_video_sample_cb(&ctx->session, video_cb, ctx);
	ChiakiAudioSink audio_sink = { .user = ctx, .header_cb = audio_header_cb, .frame_cb = audio_frame_cb };
	chiaki_session_set_audio_sink(&ctx->session, &audio_sink);
	ChiakiAudioSink haptics_sink = { .user = ctx, .header_cb = haptics_header_cb, .frame_cb = haptics_frame_cb };
	chiaki_session_set_haptics_sink(&ctx->session, &haptics_sink);

	double start_t = now_s();
	err = chiaki_session_start(&ctx->session);
	if(err != CHIAKI_ERR_SUCCESS)
	{
		ylog('E', "会话启动失败：%s", chiaki_error_string(err));
		chiaki_session_fini(&ctx->session);
		return 1;
	}

	ChiakiControllerState idle;
	chiaki_controller_state_set_idle(&idle);
	bool idle_sent = false;
	int loss_i = 0, idr_i = 0;
	double last_report = now_s(), next_wiggle = 0;
	uint64_t last_frames = 0, last_bytes = 0;
	bool reported_first_frame = false;

	while(!ctx->quit && !g_interrupted)
	{
		usleep(50 * 1000);
		double t = now_s();
		if(!ctx->connected)
		{
			if(t - start_t > 60)
			{
				ylog('E', "60 秒内没有连上，放弃");
				break;
			}
			continue;
		}
		double rel = t - ctx->connect_t; // 实验时间表以「连上」为 0 点
		if(!idle_sent)
		{
			chiaki_session_set_controller_state(&ctx->session, &idle);
			idle_sent = true;
			next_wiggle = o->wiggle > 0 ? o->wiggle : 0;
		}
		if(!reported_first_frame && ctx->frames > 0)
		{
			reported_first_frame = true;
			ylog('I', "[yw-run] 首帧：发起会话后 %.3f s，连上后 %.3f s", ctx->first_frame_t - start_t, ctx->first_frame_t - ctx->connect_t);
		}
		while(loss_i < loss_n && rel >= loss_steps[loss_i].t)
		{
			chiaki_session_yw_set_inject_loss(&ctx->session, loss_steps[loss_i].value);
			ylog('I', "[yw-plan] t=%.1f 注入丢包上报 %s%.3f", rel,
				loss_steps[loss_i].value < 0 ? "关闭 " : "", loss_steps[loss_i].value);
			loss_i++;
		}
		while(idr_i < idr_n && rel >= idr_steps[idr_i].t)
		{
			ChiakiErrorCode e = chiaki_session_request_idr(&ctx->session);
			ylog('I', "[yw-plan] t=%.1f 请求 IDR：%s（已收关键帧 %llu）", rel, chiaki_error_string(e),
				(unsigned long long)ctx->keyframes);
			idr_i++;
		}
		if(ctl_fd >= 0)
		{
			ssize_t n = read(ctl_fd, ctl_buf + ctl_len, sizeof(ctl_buf) - 1 - ctl_len);
			if(n > 0)
				ctl_len += (size_t)n;
			char *nl;
			while(!ctl_quit && (nl = memchr(ctl_buf, '\n', ctl_len)))
			{
				*nl = '\0';
				char line[1024];
				snprintf(line, sizeof(line), "%s", ctl_buf);
				size_t used = (size_t)(nl - ctl_buf) + 1;
				memmove(ctl_buf, ctl_buf + used, ctl_len - used);
				ctl_len -= used;
				if(!ctl_handle(ctx, line, rel, &idle))
					ctl_quit = true;
			}
			if(ctl_len >= sizeof(ctl_buf) - 1)
				ctl_len = 0; // 没有换行的超长垃圾，丢掉
			if(ctl_quit)
				break;
		}
		if(o->wiggle > 0 && rel >= next_wiggle)
		{
			// 轻推一下左摇杆再放开，模拟「有人在玩」，用于 AFK 实验
			ChiakiControllerState s = idle;
			s.left_x = 6000;
			chiaki_session_set_controller_state(&ctx->session, &s);
			usleep(100 * 1000);
			chiaki_session_set_controller_state(&ctx->session, &idle);
			ylog('V', "[yw-plan] t=%.1f 轻推摇杆", rel);
			next_wiggle += o->wiggle;
		}
		if(t - last_report >= 1.0)
		{
			uint64_t f = ctx->frames, b = ctx->bytes;
			double dt = t - last_report;
			ylog('I', "[yw-sec] t=%.0f fps=%.1f mbps=%.2f 关键帧=%llu 丢帧=%lld FEC恢复=%llu 音频帧=%llu 触觉帧=%llu",
				rel, (double)(f - last_frames) / dt, (double)(b - last_bytes) * 8 / dt / 1e6,
				(unsigned long long)ctx->keyframes, (long long)ctx->frames_lost, (unsigned long long)ctx->frames_recovered,
				(unsigned long long)ctx->audio_frames, (unsigned long long)ctx->haptic_frames);
			last_frames = f;
			last_bytes = b;
			last_report = t;
		}
		if(o->duration > 0 && rel >= o->duration)
		{
			ylog('I', "[yw-run] 到达设定时长，结束");
			break;
		}
	}
	if(g_interrupted)
		ylog('I', "[yw-run] 收到中断，结束");

	chiaki_session_stop(&ctx->session);
	chiaki_session_join(&ctx->session);
	chiaki_session_fini(&ctx->session);
	if(ctx->dump)
		fclose(ctx->dump);
	if(ctx->snap_file)
		fclose(ctx->snap_file);
	free(ctx->header);
	if(ctl_fd >= 0)
		close(ctl_fd);
	if(ctl_keep >= 0)
		close(ctl_keep);
	ylog('I', "[yw-run] 汇总：帧 %llu，关键帧 %llu，丢帧 %lld，FEC 恢复 %llu，日志 %s",
		(unsigned long long)ctx->frames, (unsigned long long)ctx->keyframes, (long long)ctx->frames_lost,
		(unsigned long long)ctx->frames_recovered, log_path);
	free(ctx);
	fclose(g_log_file);
	g_log_file = NULL;
	return 0;
}

// ---------------------------------------------------------------- main

static void usage(void)
{
	fprintf(stderr,
		"用法：\n"
		"  yw-m0-probe status   <主机>\n"
		"  yw-m0-probe wakeup   <主机> [--cred 文件]\n"
		"  yw-m0-probe register <主机> --pin 8位数字 --account 账号ID [--cred 文件] [--log 文件]\n"
		"  yw-m0-probe stream   <主机> [--cred 文件] [--res 1080|720|540|360] [--fps 60|30]\n"
		"                       [--bitrate kbps] [--codec h265|h264|hdr] [--duration 秒]\n"
		"                       [--loss-max 0.05] [--loss-plan 秒:丢包率,...（-1 关闭）]\n"
		"                       [--idr-at 秒,秒,...] [--wiggle 秒] [--console-pin 4位]\n"
		"                       [--dump 码流文件] [--log 日志文件] [--ctl 控制管道]\n"
		"                       [--ls-mtu 字节] [--ls-rtt 毫秒]（覆盖 LaunchSpec，E3 用）\n");
}

int main(int argc, char **argv)
{
	g_t0_us = chiaki_time_now_monotonic_us();
	if(argc < 3)
	{
		usage();
		return 1;
	}
	signal(SIGINT, on_sigint);
	signal(SIGTERM, on_sigint);
	signal(SIGPIPE, SIG_IGN);

	const char *cmd = argv[1];
	const char *host = argv[2];
	char cred_default[1024];
	default_cred_path(cred_default, sizeof(cred_default));

	StreamOpts o = {
		.host = host, .cred_path = cred_default, .res = 1080, .fps = 60,
		.duration = 60, .loss_max = 0.05
	};
	const char *pin = NULL, *account = NULL;

	static struct option longopts[] = {
		{ "cred", required_argument, NULL, 'c' },
		{ "log", required_argument, NULL, 'l' },
		{ "dump", required_argument, NULL, 'd' },
		{ "res", required_argument, NULL, 'r' },
		{ "fps", required_argument, NULL, 'f' },
		{ "bitrate", required_argument, NULL, 'b' },
		{ "codec", required_argument, NULL, 'k' },
		{ "duration", required_argument, NULL, 't' },
		{ "loss-max", required_argument, NULL, 'm' },
		{ "loss-plan", required_argument, NULL, 'p' },
		{ "idr-at", required_argument, NULL, 'i' },
		{ "wiggle", required_argument, NULL, 'w' },
		{ "console-pin", required_argument, NULL, 'C' },
		{ "pin", required_argument, NULL, 'P' },
		{ "account", required_argument, NULL, 'A' },
		{ "ctl", required_argument, NULL, 'X' },
		{ "ls-mtu", required_argument, NULL, 'U' },
		{ "ls-rtt", required_argument, NULL, 'R' },
		{ NULL, 0, NULL, 0 }
	};
	optind = 3;
	int ch;
	while((ch = getopt_long(argc, argv, "", longopts, NULL)) != -1)
	{
		switch(ch)
		{
			case 'c': o.cred_path = optarg; break;
			case 'l': o.log_path = optarg; break;
			case 'd': o.dump_path = optarg; break;
			case 'r': o.res = atoi(optarg); break;
			case 'f': o.fps = atoi(optarg); break;
			case 'b': o.bitrate = atoi(optarg); break;
			case 'k': o.codec = optarg; break;
			case 't': o.duration = atof(optarg); break;
			case 'm': o.loss_max = atof(optarg); break;
			case 'p': o.loss_plan = optarg; break;
			case 'i': o.idr_plan = optarg; break;
			case 'w': o.wiggle = atof(optarg); break;
			case 'C': o.console_pin = (uint32_t)strtoul(optarg, NULL, 10); break;
			case 'P': pin = optarg; break;
			case 'A': account = optarg; break;
			case 'X': o.ctl_path = optarg; break;
			case 'U': o.ls_mtu = (uint32_t)strtoul(optarg, NULL, 10); break;
			case 'R': o.ls_rtt = (uint32_t)strtoul(optarg, NULL, 10); break;
			default: usage(); return 1;
		}
	}

	if(!strcmp(cmd, "status"))
		return cmd_status(host);
	if(!strcmp(cmd, "wakeup"))
		return cmd_wakeup(host, o.cred_path);
	if(!strcmp(cmd, "register"))
		return cmd_register(host, pin, account, o.cred_path, o.log_path);
	if(!strcmp(cmd, "stream"))
		return cmd_stream(&o);
	usage();
	return 1;
}
