// SPDX-License-Identifier: AGPL-3.0-only
// yw-probe：链路测量工具（M0 实验 E8）。不依赖 chiaki，只用系统库。
//
// 模拟串流的下行媒体流量：服务端按设定码率、包长、发包模式（匀速 / 按帧突发）发 UDP，
// 客户端记录每个包的到达，统计：
//   丢包率、突发丢包长度分布、Gilbert 模型参数（可直接交给 Linux netem 回放）、
//   乱序、重复、单向时延变化（排队）、RFC 3550 抖动、每秒丢包、
//   空载与满载的 RTT（同一个 socket 上每 100 ms 一次 ping）。
// 另有 mtu 子命令，用 DF 包分别探测上行、下行能通过的最大 UDP 载荷。
//
// 用法：
//   yw-probe server [--port 43210] [--bind-if en0]
//   yw-probe client <服务端IP> [--port 43210] [--bind-if en0] [--rate 15M] [--size 1200]
//                   [--duration 60] [--pattern cbr|frame] [--fps 60] [--burst-us 0]
//                   [--dir down|up] [--sim-loss p,r] [--trace 文件] [--timeline 文件]
//   yw-probe mtu    <服务端IP> [--port 43210] [--bind-if en0]
//
// 两端都是小端（Apple 芯片），包头直接按结构体收发。

#include <arpa/inet.h>
#include <errno.h>
#include <getopt.h>
#include <math.h>
#include <net/if.h>
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
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

#ifndef IP_BOUND_IF
#define IP_BOUND_IF 25
#endif
#ifndef IP_DONTFRAG
#define IP_DONTFRAG 28
#endif

#define MAGIC 0x42505759u // "YWPB"
#define MAX_PKT 9000
#define NONE_U32 0xffffffffu

enum
{
	T_START = 1,
	T_START_ACK,
	T_DATA,
	T_END,
	T_PING,
	T_PONG,
	T_MTU_PROBE,
	T_MTU_ACK,
	T_REPORT,
	T_STOP,
};

typedef struct
{
	uint32_t magic;
	uint8_t type;
	uint8_t flags;
	uint16_t len;     // T_MTU_PROBE：要求对端回复的载荷长度；T_REPORT：本片文本长度
	uint32_t session;
	uint32_t seq;
	uint64_t ts;      // T_DATA：发送方自测试开始的微秒数；T_PING/T_PONG：发起方时间戳
} Hdr;

enum { PAT_CBR = 0, PAT_FRAME = 1 };
enum { DIR_DOWN = 0, DIR_UP = 1 };

typedef struct
{
	uint64_t rate_bps;
	uint32_t size;        // 整个 UDP 载荷长度（含包头）
	uint32_t duration_ms;
	uint8_t pattern;
	uint8_t dir;
	uint16_t fps;
	uint32_t burst_us;    // 按帧模式：一帧的包在多少微秒内发完（0 = 连发）
	uint32_t sim_p_ppm;   // 自测用：发送端按 Gilbert 模型故意不发（好→坏概率，百万分之）
	uint32_t sim_r_ppm;   // 坏→好概率
} Params;

static volatile sig_atomic_t g_stop = 0;
static void on_sig(int s) { (void)s; g_stop = 1; }

static uint64_t now_us(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000000ull + (uint64_t)ts.tv_nsec / 1000;
}

static uint64_t now_ns(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

// 睡到绝对时刻：先 nanosleep 到差 100 微秒，再自旋，兼顾精度和 CPU
static void sleep_until_ns(uint64_t t)
{
	for(;;)
	{
		uint64_t n = now_ns();
		if(n >= t)
			return;
		uint64_t d = t - n;
		if(d > 200000)
		{
			struct timespec ts = { .tv_sec = (time_t)((d - 100000) / 1000000000ull), .tv_nsec = (long)((d - 100000) % 1000000000ull) };
			nanosleep(&ts, NULL);
		}
	}
}

static uint64_t rng_state = 0x9e3779b97f4a7c15ull;
static uint32_t rng_u32(void)
{
	rng_state ^= rng_state << 13;
	rng_state ^= rng_state >> 7;
	rng_state ^= rng_state << 17;
	return (uint32_t)(rng_state >> 16);
}

static int make_socket(int port, const char *bind_if)
{
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	if(fd < 0)
	{
		perror("socket");
		exit(1);
	}
	int buf = 8 * 1024 * 1024;
	setsockopt(fd, SOL_SOCKET, SO_RCVBUF, &buf, sizeof(buf));
	setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &buf, sizeof(buf));
	if(bind_if && *bind_if)
	{
		unsigned int idx = if_nametoindex(bind_if);
		if(!idx || setsockopt(fd, IPPROTO_IP, IP_BOUND_IF, &idx, sizeof(idx)) != 0)
		{
			fprintf(stderr, "绑定网卡 %s 失败：%s\n", bind_if, strerror(errno));
			exit(1);
		}
	}
	struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons((uint16_t)port), .sin_addr.s_addr = htonl(INADDR_ANY) };
	if(bind(fd, (struct sockaddr *)&a, sizeof(a)) != 0)
	{
		fprintf(stderr, "bind 端口 %d 失败：%s\n", port, strerror(errno));
		exit(1);
	}
	return fd;
}

static void set_rcv_timeout(int fd, int ms)
{
	struct timeval tv = { .tv_sec = ms / 1000, .tv_usec = (ms % 1000) * 1000 };
	setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
}

static void send_hdr(int fd, const struct sockaddr_in *to, uint8_t type, uint32_t session, uint32_t seq, uint64_t ts, uint16_t len)
{
	Hdr h = { .magic = MAGIC, .type = type, .session = session, .seq = seq, .ts = ts, .len = len };
	sendto(fd, &h, sizeof(h), 0, (const struct sockaddr *)to, sizeof(*to));
}

// ---------------------------------------------------------------- 发送

typedef struct
{
	int fd;
	struct sockaddr_in to;
	uint32_t session;
	Params p;
	volatile bool *stop;
	uint64_t sent;       // 实际调用 sendto 成功的包数
	uint64_t total;      // 序号总数（含模拟丢弃的）
	uint64_t send_err;
	uint64_t sim_dropped;
} Sender;

static bool sim_should_drop(Sender *s, bool *bad)
{
	if(!s->p.sim_p_ppm)
		return false;
	uint32_t x = rng_u32() % 1000000;
	if(!*bad)
	{
		if(x < s->p.sim_p_ppm)
			*bad = true;
	}
	else if(x < s->p.sim_r_ppm)
		*bad = false;
	return *bad;
}

static void send_data(Sender *s, uint8_t *pkt, uint32_t seq, uint64_t t0_us, bool *bad)
{
	Hdr *h = (Hdr *)pkt;
	h->seq = seq;
	h->ts = now_us() - t0_us;
	s->total++;
	if(sim_should_drop(s, bad))
	{
		s->sim_dropped++;
		return;
	}
	ssize_t n = sendto(s->fd, pkt, s->p.size, 0, (struct sockaddr *)&s->to, sizeof(s->to));
	if(n < 0)
		s->send_err++;
	else
		s->sent++;
}

static void *sender_thread(void *user)
{
	Sender *s = user;
	uint8_t pkt[MAX_PKT];
	memset(pkt, 0xa5, sizeof(pkt));
	Hdr *h = (Hdr *)pkt;
	h->magic = MAGIC;
	h->type = T_DATA;
	h->flags = 0;
	h->len = 0;
	h->session = s->session;
	bool bad = false;

	uint64_t t0_us = now_us();
	uint64_t t0 = now_ns();
	uint64_t end = t0 + (uint64_t)s->p.duration_ms * 1000000ull;
	uint32_t seq = 0;
	if(s->p.pattern == PAT_CBR)
	{
		double interval = (double)s->p.size * 8.0 * 1e9 / (double)s->p.rate_bps;
		uint64_t k = 0;
		while(!*s->stop && !g_stop)
		{
			uint64_t t = t0 + (uint64_t)(k * interval);
			if(t >= end)
				break;
			sleep_until_ns(t);
			send_data(s, pkt, seq++, t0_us, &bad);
			k++;
		}
	}
	else
	{
		uint64_t frame_ns = 1000000000ull / (s->p.fps ? s->p.fps : 60);
		double bytes_per_frame = (double)s->p.rate_bps / 8.0 / (double)(s->p.fps ? s->p.fps : 60);
		double carry = 0;
		uint64_t f = 0;
		while(!*s->stop && !g_stop)
		{
			uint64_t t = t0 + f * frame_ns;
			if(t >= end)
				break;
			sleep_until_ns(t);
			carry += bytes_per_frame / (double)s->p.size;
			uint32_t n = (uint32_t)carry;
			carry -= n;
			for(uint32_t i = 0; i < n && !*s->stop; i++)
			{
				if(s->p.burst_us && n > 1)
					sleep_until_ns(t + (uint64_t)s->p.burst_us * 1000ull * i / (n - 1));
				send_data(s, pkt, seq++, t0_us, &bad);
			}
			f++;
		}
	}
	for(int i = 0; i < 5; i++)
	{
		send_hdr(s->fd, &s->to, T_END, s->session, seq, 0, 0);
		usleep(20000);
	}
	return NULL;
}

// ---------------------------------------------------------------- 接收与统计

typedef struct
{
	uint32_t *recv_us;   // 每个序号的到达时刻（相对接收开始，微秒），NONE_U32 = 没收到
	uint32_t *send_us;   // 每个序号的发送时刻（发送方相对时间）
	size_t cap;
	uint64_t t0_us;
	uint64_t received, duplicates, reordered, bytes;
	int64_t max_seq;
	uint32_t max_reorder;
	double jitter_us;
	bool have_prev;
	uint64_t prev_r, prev_s;
	int64_t total;       // 来自 END；-1 表示没收到 END
	uint64_t first_recv_us, last_recv_us;
} Recv;

static void recv_init(Recv *r, size_t cap)
{
	memset(r, 0, sizeof(*r));
	r->cap = cap < 1024 ? 1024 : cap;
	r->recv_us = malloc(r->cap * sizeof(uint32_t));
	r->send_us = malloc(r->cap * sizeof(uint32_t));
	memset(r->recv_us, 0xff, r->cap * sizeof(uint32_t));
	r->max_seq = -1;
	r->total = -1;
	r->t0_us = now_us();
}

static void recv_fini(Recv *r)
{
	free(r->recv_us);
	free(r->send_us);
}

static void recv_data(Recv *r, const Hdr *h, size_t n)
{
	uint64_t t = now_us() - r->t0_us;
	uint32_t seq = h->seq;
	if(seq >= r->cap)
	{
		size_t nc = r->cap;
		while(nc <= seq)
			nc *= 2;
		r->recv_us = realloc(r->recv_us, nc * sizeof(uint32_t));
		r->send_us = realloc(r->send_us, nc * sizeof(uint32_t));
		memset(r->recv_us + r->cap, 0xff, (nc - r->cap) * sizeof(uint32_t));
		r->cap = nc;
	}
	if(r->recv_us[seq] != NONE_U32)
	{
		r->duplicates++;
		return;
	}
	r->recv_us[seq] = (uint32_t)t;
	r->send_us[seq] = (uint32_t)h->ts;
	r->received++;
	r->bytes += n;
	if(r->received == 1)
		r->first_recv_us = t;
	r->last_recv_us = t;
	if((int64_t)seq < r->max_seq)
	{
		r->reordered++;
		uint32_t depth = (uint32_t)(r->max_seq - seq);
		if(depth > r->max_reorder)
			r->max_reorder = depth;
	}
	else
		r->max_seq = seq;
	// RFC 3550 到达间隔抖动（按到达顺序）
	if(r->have_prev)
	{
		double d = ((double)t - (double)r->prev_r) - ((double)h->ts - (double)r->prev_s);
		r->jitter_us += (fabs(d) - r->jitter_us) / 16.0;
	}
	r->prev_r = t;
	r->prev_s = h->ts;
	r->have_prev = true;
}

static int cmp_u32(const void *a, const void *b)
{
	uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
	return x < y ? -1 : x > y;
}

static int cmp_double(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return x < y ? -1 : x > y;
}

static uint32_t pct_u32(uint32_t *v, size_t n, double p)
{
	if(!n)
		return 0;
	size_t k = (size_t)llround(p / 100.0 * (double)(n - 1));
	return v[k];
}

typedef struct
{
	double rtt[100000];
	size_t n;
	pthread_mutex_t m;
} Rtts;

static double rtt_pct(Rtts *r, double p)
{
	if(!r->n)
		return NAN;
	double *v = malloc(r->n * sizeof(double));
	memcpy(v, r->rtt, r->n * sizeof(double));
	qsort(v, r->n, sizeof(double), cmp_double);
	double x = v[(size_t)llround(p / 100.0 * (double)(r->n - 1))];
	free(v);
	return x;
}

// 生成统计汇总（中文文本），同时可写逐包 trace 和每秒 timeline
static void summarize(Recv *r, const Params *p, Rtts *idle, Rtts *load, uint64_t pings_sent,
	const char *trace_path, const char *timeline_path, char *out, size_t out_size)
{
	int64_t total = r->total >= 0 ? r->total : r->max_seq + 1;
	if(total <= 0)
	{
		snprintf(out, out_size, "没有收到任何数据包。\n");
		return;
	}
	if((size_t)total > r->cap)
		total = (int64_t)r->cap;
	uint64_t lost = 0, runs = 0, max_run = 0, cur = 0;
	uint64_t hist[7] = { 0 }; // 1, 2, 3-4, 5-8, 9-16, 17-64, >64
	for(int64_t s = 0; s <= total; s++)
	{
		bool miss = s < total && r->recv_us[s] == NONE_U32;
		if(miss)
		{
			lost++;
			cur++;
		}
		else if(cur)
		{
			runs++;
			if(cur > max_run)
				max_run = cur;
			int b = cur == 1 ? 0 : cur == 2 ? 1 : cur <= 4 ? 2 : cur <= 8 ? 3 : cur <= 16 ? 4 : cur <= 64 ? 5 : 6;
			hist[b]++;
			cur = 0;
		}
	}
	uint64_t got = (uint64_t)total - lost;
	double loss = (double)lost / (double)total;
	double gp = got ? (double)runs / (double)got : 0;   // 好→坏
	double gr = lost ? (double)runs / (double)lost : 0; // 坏→好

	// 单向时延变化：按 10 秒窗口（按发送时刻）减去窗口最小值，抵消两端时钟偏差和漂移
	size_t nwin = (size_t)(p->duration_ms / 10000) + 3;
	int64_t *wmin = malloc(nwin * sizeof(int64_t));
	for(size_t i = 0; i < nwin; i++)
		wmin[i] = INT64_MAX;
	for(int64_t s = 0; s < total; s++)
	{
		if(r->recv_us[s] == NONE_U32)
			continue;
		size_t w = r->send_us[s] / 10000000u;
		if(w >= nwin)
			w = nwin - 1;
		int64_t d = (int64_t)r->recv_us[s] - (int64_t)r->send_us[s];
		if(d < wmin[w])
			wmin[w] = d;
	}
	uint32_t *dv = malloc((got ? got : 1) * sizeof(uint32_t));
	size_t nd = 0;
	for(int64_t s = 0; s < total; s++)
	{
		if(r->recv_us[s] == NONE_U32)
			continue;
		size_t w = r->send_us[s] / 10000000u;
		if(w >= nwin)
			w = nwin - 1;
		int64_t d = (int64_t)r->recv_us[s] - (int64_t)r->send_us[s] - wmin[w];
		dv[nd++] = d < 0 ? 0 : (uint32_t)d;
	}
	qsort(dv, nd, sizeof(uint32_t), cmp_u32);

	// 每秒丢包（按序号折算成发送时间）
	double pps = (double)p->rate_bps / ((double)p->size * 8.0);
	size_t nsec = (size_t)((double)total / pps) + 2;
	uint32_t *sec_sent = calloc(nsec, sizeof(uint32_t));
	uint32_t *sec_got = calloc(nsec, sizeof(uint32_t));
	for(int64_t s = 0; s < total; s++)
	{
		size_t b = (size_t)((double)s / pps);
		if(b >= nsec)
			b = nsec - 1;
		sec_sent[b]++;
		if(r->recv_us[s] != NONE_U32)
			sec_got[b]++;
	}
	double *sec_loss = malloc(nsec * sizeof(double));
	size_t nsl = 0, lossy_secs = 0;
	double worst = 0;
	size_t worst_i = 0;
	for(size_t i = 0; i < nsec; i++)
	{
		if(sec_sent[i] < pps * 0.5)
			continue; // 首尾不完整的秒不计
		double l = 1.0 - (double)sec_got[i] / sec_sent[i];
		sec_loss[nsl++] = l;
		if(l > 0)
			lossy_secs++;
		if(l > worst)
		{
			worst = l;
			worst_i = i;
		}
	}
	qsort(sec_loss, nsl, sizeof(double), cmp_double);
	double sec_p95 = nsl ? sec_loss[(size_t)llround(0.95 * (double)(nsl - 1))] : 0;

	if(timeline_path)
	{
		FILE *f = fopen(timeline_path, "w");
		if(f)
		{
			fprintf(f, "sec,sent,received,loss\n");
			for(size_t i = 0; i < nsec; i++)
				fprintf(f, "%zu,%u,%u,%.5f\n", i, sec_sent[i], sec_got[i], sec_sent[i] ? 1.0 - (double)sec_got[i] / sec_sent[i] : 0.0);
			fclose(f);
		}
	}
	if(trace_path)
	{
		FILE *f = fopen(trace_path, "w");
		if(f)
		{
			fprintf(f, "seq,send_us,recv_us\n");
			for(int64_t s = 0; s < total; s++)
			{
				if(r->recv_us[s] == NONE_U32)
					fprintf(f, "%lld,,\n", (long long)s);
				else
					fprintf(f, "%lld,%u,%u\n", (long long)s, r->send_us[s], r->recv_us[s]);
			}
			fclose(f);
		}
	}

	double secs = (double)(r->last_recv_us - r->first_recv_us) / 1e6;
	double mbps = secs > 0 ? (double)r->bytes * 8.0 / secs / 1e6 : 0;
	size_t o = 0;
	o += (size_t)snprintf(out + o, out_size - o,
		"发送 %lld 包%s，收到 %llu，丢失 %llu（%.3f%%），重复 %llu，乱序 %llu（最大深度 %u）\n"
		"收到的平均码率 %.2f Mbps（%.0f 秒）\n"
		"丢包段 %llu 个，平均长度 %.2f，最长 %llu；长度分布 1:%llu 2:%llu 3-4:%llu 5-8:%llu 9-16:%llu 17-64:%llu >64:%llu\n"
		"Gilbert 模型：好→坏 p=%.4f%%，坏→好 r=%.2f%%（netem：loss gemodel %.4f%% %.2f%% 100%% 0%%）\n"
		"每秒丢包：有丢包的秒 %zu/%zu，P95 %.3f%%，最差 %.3f%%（第 %zu 秒）\n"
		"单向时延变化（排队）P50 %.2f ms，P95 %.2f ms，P99 %.2f ms，最大 %.2f ms；RFC3550 抖动 %.3f ms\n",
		(long long)total, r->total >= 0 ? "" : "（没收到结束包，按最大序号估计）",
		(unsigned long long)got, (unsigned long long)lost, loss * 100, (unsigned long long)r->duplicates,
		(unsigned long long)r->reordered, r->max_reorder, mbps, secs,
		(unsigned long long)runs, runs ? (double)lost / runs : 0, (unsigned long long)max_run,
		(unsigned long long)hist[0], (unsigned long long)hist[1], (unsigned long long)hist[2], (unsigned long long)hist[3],
		(unsigned long long)hist[4], (unsigned long long)hist[5], (unsigned long long)hist[6],
		gp * 100, gr * 100, gp * 100, gr * 100,
		lossy_secs, nsl, sec_p95 * 100, worst * 100, worst_i,
		pct_u32(dv, nd, 50) / 1000.0, pct_u32(dv, nd, 95) / 1000.0, pct_u32(dv, nd, 99) / 1000.0,
		nd ? dv[nd - 1] / 1000.0 : 0, r->jitter_us / 1000.0);
	if(idle && load)
	{
		o += (size_t)snprintf(out + o, out_size - o,
			"RTT 空载：P50 %.2f ms，最小 %.2f ms（%zu 次）；满载：P50 %.2f ms，P95 %.2f ms，P99 %.2f ms，最大 %.2f ms（%zu/%llu 次有回应）\n",
			rtt_pct(idle, 50), rtt_pct(idle, 0), idle->n,
			rtt_pct(load, 50), rtt_pct(load, 95), rtt_pct(load, 99), rtt_pct(load, 100), load->n, (unsigned long long)pings_sent);
	}
	snprintf(out + o, out_size - o,
		"[yw-probe-json] {\"total\":%lld,\"lost\":%llu,\"loss\":%.6f,\"runs\":%llu,\"max_run\":%llu,\"gp\":%.6f,\"gr\":%.6f,"
		"\"reordered\":%llu,\"dup\":%llu,\"mbps\":%.3f,\"owd_p50_ms\":%.3f,\"owd_p95_ms\":%.3f,\"owd_p99_ms\":%.3f,\"jitter_ms\":%.3f,"
		"\"lossy_secs\":%zu,\"secs\":%zu,\"worst_sec_loss\":%.5f}\n",
		(long long)total, (unsigned long long)lost, loss, (unsigned long long)runs, (unsigned long long)max_run, gp, gr,
		(unsigned long long)r->reordered, (unsigned long long)r->duplicates, mbps,
		pct_u32(dv, nd, 50) / 1000.0, pct_u32(dv, nd, 95) / 1000.0, pct_u32(dv, nd, 99) / 1000.0, r->jitter_us / 1000.0,
		lossy_secs, nsl, worst);

	free(wmin);
	free(dv);
	free(sec_sent);
	free(sec_got);
	free(sec_loss);
}

// ---------------------------------------------------------------- 服务端

typedef struct
{
	bool active;
	uint32_t session;
	struct sockaddr_in peer;
	Params p;
	uint64_t last_seen_us;
	uint64_t start_us;
	// 下行：发送线程
	pthread_t thread;
	bool thread_running;
	volatile bool stop;
	Sender sender;
	// 上行：接收统计
	Recv recv;
	bool recv_active;
	bool ended;
	uint64_t end_us;
} Session;

static void session_stop(Session *s)
{
	if(s->thread_running)
	{
		s->stop = true;
		pthread_join(s->thread, NULL);
		s->thread_running = false;
		fprintf(stderr, "会话 %08x 下行发送结束：%llu 包，发送错误 %llu，模拟丢弃 %llu\n", s->session,
			(unsigned long long)s->sender.sent, (unsigned long long)s->sender.send_err, (unsigned long long)s->sender.sim_dropped);
	}
	if(s->recv_active)
	{
		recv_fini(&s->recv);
		s->recv_active = false;
	}
	s->active = false;
}

static void send_report(int fd, const struct sockaddr_in *to, uint32_t session, const char *text)
{
	size_t len = strlen(text), off = 0;
	uint32_t part = 0;
	uint8_t pkt[1400];
	while(off < len)
	{
		size_t n = len - off > 1200 ? 1200 : len - off;
		Hdr h = { .magic = MAGIC, .type = T_REPORT, .session = session, .seq = part++, .len = (uint16_t)n,
			.flags = (off + n >= len) ? 1 : 0 };
		memcpy(pkt, &h, sizeof(h));
		memcpy(pkt + sizeof(h), text + off, n);
		sendto(fd, pkt, sizeof(h) + n, 0, (const struct sockaddr *)to, sizeof(*to));
		off += n;
	}
}

static int cmd_server(int port, const char *bind_if)
{
	int fd = make_socket(port, bind_if);
	set_rcv_timeout(fd, 100);
	fprintf(stderr, "yw-probe 服务端监听 UDP %d%s%s\n", port, bind_if ? "，绑定网卡 " : "", bind_if ? bind_if : "");
	Session s;
	memset(&s, 0, sizeof(s));
	uint8_t buf[MAX_PKT + 64];
	while(!g_stop)
	{
		struct sockaddr_in from;
		socklen_t fl = sizeof(from);
		ssize_t n = recvfrom(fd, buf, sizeof(buf), 0, (struct sockaddr *)&from, &fl);
		uint64_t t = now_us();
		if(n >= (ssize_t)sizeof(Hdr))
		{
			Hdr h;
			memcpy(&h, buf, sizeof(h));
			if(h.magic != MAGIC)
				continue;
			// 记录上行静默：同一会话两次收到客户端消息的间隔超过 0.5 秒
			if(s.active && h.session == s.session && h.type != T_START && s.last_seen_us && t - s.last_seen_us > 500000)
				fprintf(stderr, "会话 %08x 上行静默 %.2f 秒（会话第 %.1f 秒恢复）\n", s.session,
					(double)(t - s.last_seen_us) / 1e6, (double)(t - s.start_us) / 1e6);
			switch(h.type)
			{
				case T_START:
				{
					if((size_t)n < sizeof(Hdr) + sizeof(Params))
						break;
					if(s.active && s.session == h.session)
					{
						send_hdr(fd, &from, T_START_ACK, h.session, 0, 0, 0);
						break;
					}
					if(s.active)
						session_stop(&s);
					memset(&s, 0, sizeof(s));
					s.active = true;
					s.session = h.session;
					s.peer = from;
					memcpy(&s.p, buf + sizeof(Hdr), sizeof(Params));
					if(s.p.size < sizeof(Hdr) || s.p.size > MAX_PKT)
						s.p.size = 1200;
					s.last_seen_us = t;
					s.start_us = t;
					char ip[32];
					inet_ntop(AF_INET, &from.sin_addr, ip, sizeof(ip));
					fprintf(stderr, "会话 %08x 来自 %s:%u：%s，%.2f Mbps，包长 %u，%u 秒，%s\n", h.session, ip,
						ntohs(from.sin_port), s.p.dir == DIR_DOWN ? "下行（本端发）" : "上行（本端收）",
						s.p.rate_bps / 1e6, s.p.size, s.p.duration_ms / 1000, s.p.pattern == PAT_CBR ? "匀速" : "按帧突发");
					send_hdr(fd, &from, T_START_ACK, h.session, 0, 0, 0);
					if(s.p.dir == DIR_DOWN)
					{
						s.sender = (Sender){ .fd = fd, .to = from, .session = h.session, .p = s.p, .stop = &s.stop };
						s.stop = false;
						pthread_create(&s.thread, NULL, sender_thread, &s.sender);
						s.thread_running = true;
					}
					else
					{
						double pps = (double)s.p.rate_bps / ((double)s.p.size * 8.0);
						recv_init(&s.recv, (size_t)(pps * s.p.duration_ms / 1000.0 * 1.1) + 1024);
						s.recv_active = true;
					}
					break;
				}
				case T_PING:
					send_hdr(fd, &from, T_PONG, h.session, h.seq, h.ts, 0);
					if(s.active && h.session == s.session)
						s.last_seen_us = t;
					break;
				case T_DATA:
					if(s.active && s.recv_active && h.session == s.session)
					{
						recv_data(&s.recv, (Hdr *)buf, (size_t)n);
						s.last_seen_us = t;
					}
					break;
				case T_END:
					if(s.active && s.recv_active && h.session == s.session && !s.ended)
					{
						s.recv.total = h.seq;
						s.ended = true;
						s.end_us = t;
					}
					break;
				case T_STOP:
					if(s.active && h.session == s.session)
					{
						fprintf(stderr, "会话 %08x 客户端要求结束\n", s.session);
						session_stop(&s);
					}
					break;
				case T_MTU_PROBE:
				{
					// 回一个指定长度、带 DF 的包，用来测下行；收到本身说明上行这个长度能通
					uint16_t want = h.len;
					if(want < sizeof(Hdr))
						want = sizeof(Hdr);
					if(want > MAX_PKT)
						want = MAX_PKT;
					uint8_t pkt[MAX_PKT];
					memset(pkt, 0x5a, want);
					Hdr r = { .magic = MAGIC, .type = T_MTU_ACK, .session = h.session, .seq = h.seq, .ts = (uint64_t)n, .len = want };
					memcpy(pkt, &r, sizeof(r));
					int on = 1;
					setsockopt(fd, IPPROTO_IP, IP_DONTFRAG, &on, sizeof(on));
					sendto(fd, pkt, want, 0, (struct sockaddr *)&from, sizeof(from));
					on = 0;
					setsockopt(fd, IPPROTO_IP, IP_DONTFRAG, &on, sizeof(on));
					break;
				}
				default:
					break;
			}
		}
		// 上行测试结束：等 0.5 秒收尾，再算统计并回报
		if(s.active && s.ended && now_us() - s.end_us > 500000)
		{
			char text[4096];
			summarize(&s.recv, &s.p, NULL, NULL, 0, NULL, NULL, text, sizeof(text));
			fprintf(stderr, "会话 %08x 上行统计：\n%s", s.session, text);
			for(int i = 0; i < 3; i++)
			{
				send_report(fd, &s.peer, s.session, text);
				usleep(50000);
			}
			session_stop(&s);
		}
		// 客户端失联 20 秒才结束会话（链路会有数秒的中断，要能测出来）
		if(s.active && now_us() - s.last_seen_us > 20000000)
		{
			fprintf(stderr, "会话 %08x 客户端 20 秒没有消息，结束\n", s.session);
			session_stop(&s);
		}
		// 下行发送已结束（线程退出）
		if(s.active && s.thread_running && s.p.dir == DIR_DOWN && now_us() - s.last_seen_us > 0)
		{
			// 线程自己结束时不需要处理，等客户端 STOP 或超时
		}
	}
	if(s.active)
		session_stop(&s);
	close(fd);
	return 0;
}

// ---------------------------------------------------------------- 客户端

typedef struct
{
	int fd;
	struct sockaddr_in srv;
	uint32_t session;
	volatile bool stop;
	volatile bool loaded;   // false：空载阶段；true：测试阶段
	Rtts *idle, *load;
	uint64_t pings_sent_load;
} Pinger;

static Rtts g_idle, g_load;

static void *pinger_thread(void *user)
{
	Pinger *pg = user;
	uint32_t seq = 0;
	while(!pg->stop && !g_stop)
	{
		send_hdr(pg->fd, &pg->srv, T_PING, pg->session, seq++ | (pg->loaded ? 0x80000000u : 0), now_us(), 0);
		if(pg->loaded)
			pg->pings_sent_load++;
		usleep(100000);
	}
	return NULL;
}

static void add_rtt(Rtts *r, double ms)
{
	pthread_mutex_lock(&r->m);
	if(r->n < sizeof(r->rtt) / sizeof(r->rtt[0]))
		r->rtt[r->n++] = ms;
	pthread_mutex_unlock(&r->m);
}

static int cmd_client(const char *host, int port, const char *bind_if, Params p, const char *trace, const char *timeline)
{
	int fd = make_socket(0, bind_if);
	set_rcv_timeout(fd, 100);
	struct sockaddr_in srv = { .sin_family = AF_INET, .sin_port = htons((uint16_t)port) };
	if(inet_pton(AF_INET, host, &srv.sin_addr) != 1)
	{
		fprintf(stderr, "服务端地址必须是 IPv4：%s\n", host);
		return 1;
	}
	rng_state ^= now_ns();
	uint32_t session = rng_u32() ^ (uint32_t)getpid();
	pthread_mutex_init(&g_idle.m, NULL);
	pthread_mutex_init(&g_load.m, NULL);

	// 1）空载 RTT：2 秒
	Pinger pg = { .fd = fd, .srv = srv, .session = session, .idle = &g_idle, .load = &g_load };
	pthread_t pth;
	pthread_create(&pth, NULL, pinger_thread, &pg);
	uint8_t buf[MAX_PKT + 64];
	uint64_t until = now_us() + 2000000;
	while(now_us() < until && !g_stop)
	{
		ssize_t n = recv(fd, buf, sizeof(buf), 0);
		if(n < (ssize_t)sizeof(Hdr))
			continue;
		Hdr h;
		memcpy(&h, buf, sizeof(h));
		if(h.magic == MAGIC && h.type == T_PONG && h.session == session)
			add_rtt(&g_idle, (double)(now_us() - h.ts) / 1000.0);
	}
	if(g_idle.n == 0)
	{
		fprintf(stderr, "2 秒内没有收到服务端的 ping 回应，检查地址、端口和路由。\n");
		pg.stop = true;
		pthread_join(pth, NULL);
		return 1;
	}

	// 2）发 START，等 ACK
	uint8_t start[sizeof(Hdr) + sizeof(Params)];
	Hdr sh = { .magic = MAGIC, .type = T_START, .session = session };
	memcpy(start, &sh, sizeof(sh));
	memcpy(start + sizeof(sh), &p, sizeof(p));
	bool acked = false;
	Recv r;
	double pps = (double)p.rate_bps / ((double)p.size * 8.0);
	recv_init(&r, (size_t)(pps * p.duration_ms / 1000.0 * 1.1) + 1024);
	for(int i = 0; i < 20 && !acked && !g_stop; i++)
	{
		sendto(fd, start, sizeof(start), 0, (struct sockaddr *)&srv, sizeof(srv));
		uint64_t wait = now_us() + 250000;
		while(now_us() < wait && !acked)
		{
			ssize_t n = recv(fd, buf, sizeof(buf), 0);
			if(n < (ssize_t)sizeof(Hdr))
				continue;
			Hdr h;
			memcpy(&h, buf, sizeof(h));
			if(h.magic != MAGIC || h.session != session)
				continue;
			if(h.type == T_START_ACK)
				acked = true;
			else if(h.type == T_PONG)
				add_rtt(&g_idle, (double)(now_us() - h.ts) / 1000.0);
			else if(h.type == T_DATA)
			{
				acked = true;
				recv_data(&r, (Hdr *)buf, (size_t)n);
			}
		}
	}
	if(!acked)
	{
		fprintf(stderr, "服务端没有确认测试请求。\n");
		pg.stop = true;
		pthread_join(pth, NULL);
		return 1;
	}
	pg.loaded = true;
	fprintf(stderr, "测试开始：%s %.2f Mbps，包长 %u，%u 秒，%s%s\n", p.dir == DIR_DOWN ? "下行" : "上行",
		p.rate_bps / 1e6, p.size, p.duration_ms / 1000, p.pattern == PAT_CBR ? "匀速" : "按帧突发",
		p.sim_p_ppm ? "（自测：模拟丢包）" : "");

	// 3）上行测试时本端发；下行测试时本端收
	Sender snd;
	pthread_t sth;
	volatile bool sstop = false;
	if(p.dir == DIR_UP)
	{
		snd = (Sender){ .fd = fd, .to = srv, .session = session, .p = p, .stop = &sstop };
		pthread_create(&sth, NULL, sender_thread, &snd);
	}
	uint64_t deadline = now_us() + (uint64_t)p.duration_ms * 1000ull + 10000000ull;
	uint64_t end_seen = 0;
	char report[8192] = "";
	size_t report_len = 0;
	uint32_t report_next_part = 0; // 统计文本按片序号依次拼接（服务端会整份重发 3 遍）
	bool report_done = false;
	uint64_t last_progress = now_us();
	while(!g_stop && now_us() < deadline)
	{
		ssize_t n = recv(fd, buf, sizeof(buf), 0);
		uint64_t t = now_us();
		if(n >= (ssize_t)sizeof(Hdr))
		{
			Hdr h;
			memcpy(&h, buf, sizeof(h));
			if(h.magic == MAGIC && h.session == session)
			{
				if(h.type == T_DATA && p.dir == DIR_DOWN)
					recv_data(&r, (Hdr *)buf, (size_t)n);
				else if(h.type == T_PONG)
					add_rtt((h.seq & 0x80000000u) ? &g_load : &g_idle, (double)(t - h.ts) / 1000.0);
				else if(h.type == T_END && p.dir == DIR_DOWN && !end_seen)
				{
					r.total = h.seq;
					end_seen = t;
				}
				else if(h.type == T_REPORT && p.dir == DIR_UP && !report_done && h.seq == report_next_part)
				{
					size_t len = h.len;
					if(report_len + len < sizeof(report) && (size_t)n >= sizeof(Hdr) + len)
					{
						memcpy(report + report_len, buf + sizeof(Hdr), len);
						report_len += len;
						report[report_len] = '\0';
						report_next_part++;
						if(h.flags & 1)
							report_done = true;
					}
				}
			}
		}
		if(p.dir == DIR_DOWN && end_seen && t - end_seen > 500000)
			break;
		if(p.dir == DIR_UP && report_done)
			break;
		if(t - last_progress >= 10000000ull)
		{
			last_progress = t;
			if(p.dir == DIR_DOWN)
				fprintf(stderr, "…已收 %llu 包\n", (unsigned long long)r.received);
		}
	}
	pg.stop = true;
	pthread_join(pth, NULL);
	if(p.dir == DIR_UP)
	{
		sstop = true;
		pthread_join(sth, NULL);
	}
	for(int i = 0; i < 3; i++)
		send_hdr(fd, &srv, T_STOP, session, 0, 0, 0);

	char text[8192];
	if(p.dir == DIR_DOWN)
	{
		summarize(&r, &p, &g_idle, &g_load, pg.pings_sent_load, trace, timeline, text, sizeof(text));
		printf("%s", text);
	}
	else
	{
		printf("上行（本端发送 %llu 包，发送错误 %llu）——服务端统计：\n%s", (unsigned long long)snd.sent,
			(unsigned long long)snd.send_err, report_done ? report : "（没收到服务端的统计）\n");
		printf("RTT 空载：P50 %.2f ms；满载：P50 %.2f ms，P95 %.2f ms，P99 %.2f ms，最大 %.2f ms\n",
			rtt_pct(&g_idle, 50), rtt_pct(&g_load, 50), rtt_pct(&g_load, 95), rtt_pct(&g_load, 99), rtt_pct(&g_load, 100));
	}
	recv_fini(&r);
	close(fd);
	return 0;
}

// ---------------------------------------------------------------- MTU

static bool mtu_try(int fd, const struct sockaddr_in *srv, uint32_t session, uint16_t up_size, uint16_t down_size, uint32_t seq)
{
	uint8_t pkt[MAX_PKT];
	memset(pkt, 0x3c, sizeof(pkt));
	Hdr h = { .magic = MAGIC, .type = T_MTU_PROBE, .session = session, .seq = seq, .len = down_size };
	memcpy(pkt, &h, sizeof(h));
	for(int attempt = 0; attempt < 3; attempt++)
	{
		if(sendto(fd, pkt, up_size, 0, (const struct sockaddr *)srv, sizeof(*srv)) < 0)
			return false; // 本机网卡 MTU 就放不下（EMSGSIZE）
		uint64_t wait = now_us() + 400000;
		while(now_us() < wait)
		{
			uint8_t buf[MAX_PKT + 64];
			ssize_t n = recv(fd, buf, sizeof(buf), 0);
			if(n < (ssize_t)sizeof(Hdr))
				continue;
			Hdr r;
			memcpy(&r, buf, sizeof(r));
			if(r.magic == MAGIC && r.type == T_MTU_ACK && r.session == session && r.seq == seq)
				return (size_t)n == down_size && r.ts == up_size;
		}
	}
	return false;
}

static int cmd_mtu(const char *host, int port, const char *bind_if)
{
	int fd = make_socket(0, bind_if);
	set_rcv_timeout(fd, 100);
	int on = 1;
	setsockopt(fd, IPPROTO_IP, IP_DONTFRAG, &on, sizeof(on));
	struct sockaddr_in srv = { .sin_family = AF_INET, .sin_port = htons((uint16_t)port) };
	if(inet_pton(AF_INET, host, &srv.sin_addr) != 1)
	{
		fprintf(stderr, "服务端地址必须是 IPv4：%s\n", host);
		return 1;
	}
	uint32_t session = (uint32_t)now_us();
	uint32_t seq = 1;
	if(!mtu_try(fd, &srv, session, 100, 100, seq++))
	{
		fprintf(stderr, "小包都不通，检查服务端。\n");
		return 1;
	}
	// 上行：二分找最大能通的载荷（回包固定小包）
	int lo = 100, hi = 1472 + 1;
	while(hi - lo > 1)
	{
		int mid = (lo + hi) / 2;
		if(mtu_try(fd, &srv, session, (uint16_t)mid, 100, seq++))
			lo = mid;
		else
			hi = mid;
	}
	int up = lo;
	lo = 100;
	hi = 1472 + 1;
	while(hi - lo > 1)
	{
		int mid = (lo + hi) / 2;
		if(mtu_try(fd, &srv, session, 100, (uint16_t)mid, seq++))
			lo = mid;
		else
			hi = mid;
	}
	int down = lo;
	printf("上行最大 UDP 载荷 %d 字节（路径 MTU ≈ %d），下行最大 UDP 载荷 %d 字节（路径 MTU ≈ %d）。上限按 1500 测。\n",
		up, up + 28, down, down + 28);
	close(fd);
	return 0;
}

// ---------------------------------------------------------------- main

static uint64_t parse_rate(const char *s)
{
	char *end;
	double v = strtod(s, &end);
	if(*end == 'k' || *end == 'K') v *= 1e3;
	else if(*end == 'm' || *end == 'M') v *= 1e6;
	else if(*end == 'g' || *end == 'G') v *= 1e9;
	return (uint64_t)v;
}

static void usage(void)
{
	fprintf(stderr,
		"用法：\n"
		"  yw-probe server [--port 43210] [--bind-if 网卡]\n"
		"  yw-probe client <服务端IP> [--port 43210] [--bind-if 网卡] [--rate 15M] [--size 1200]\n"
		"                  [--duration 60] [--pattern cbr|frame] [--fps 60] [--burst-us 0]\n"
		"                  [--dir down|up] [--sim-loss p,r] [--trace 文件] [--timeline 文件]\n"
		"  yw-probe mtu    <服务端IP> [--port 43210] [--bind-if 网卡]\n");
}

int main(int argc, char **argv)
{
	if(argc < 2)
	{
		usage();
		return 1;
	}
	signal(SIGINT, on_sig);
	signal(SIGTERM, on_sig);
	const char *cmd = argv[1];
	const char *host = NULL;
	int argi = 2;
	if(strcmp(cmd, "server") != 0)
	{
		if(argc < 3)
		{
			usage();
			return 1;
		}
		host = argv[2];
		argi = 3;
	}
	int port = 43210;
	const char *bind_if = NULL, *trace = NULL, *timeline = NULL;
	Params p = { .rate_bps = 15000000, .size = 1200, .duration_ms = 60000, .pattern = PAT_CBR, .dir = DIR_DOWN, .fps = 60 };
	static struct option lo[] = {
		{ "port", required_argument, NULL, 'p' },
		{ "bind-if", required_argument, NULL, 'b' },
		{ "rate", required_argument, NULL, 'r' },
		{ "size", required_argument, NULL, 's' },
		{ "duration", required_argument, NULL, 'd' },
		{ "pattern", required_argument, NULL, 'P' },
		{ "fps", required_argument, NULL, 'f' },
		{ "burst-us", required_argument, NULL, 'B' },
		{ "dir", required_argument, NULL, 'D' },
		{ "sim-loss", required_argument, NULL, 'S' },
		{ "trace", required_argument, NULL, 't' },
		{ "timeline", required_argument, NULL, 'T' },
		{ NULL, 0, NULL, 0 }
	};
	optind = argi;
	int ch;
	while((ch = getopt_long(argc, argv, "", lo, NULL)) != -1)
	{
		switch(ch)
		{
			case 'p': port = atoi(optarg); break;
			case 'b': bind_if = optarg; break;
			case 'r': p.rate_bps = parse_rate(optarg); break;
			case 's': p.size = (uint32_t)atoi(optarg); break;
			case 'd': p.duration_ms = (uint32_t)(atof(optarg) * 1000); break;
			case 'P': p.pattern = !strcmp(optarg, "frame") ? PAT_FRAME : PAT_CBR; break;
			case 'f': p.fps = (uint16_t)atoi(optarg); break;
			case 'B': p.burst_us = (uint32_t)atoi(optarg); break;
			case 'D': p.dir = !strcmp(optarg, "up") ? DIR_UP : DIR_DOWN; break;
			case 'S':
			{
				double sp, sr;
				if(sscanf(optarg, "%lf,%lf", &sp, &sr) != 2)
				{
					fprintf(stderr, "--sim-loss 格式：p,r（0~1）\n");
					return 1;
				}
				p.sim_p_ppm = (uint32_t)(sp * 1e6);
				p.sim_r_ppm = (uint32_t)(sr * 1e6);
				break;
			}
			case 't': trace = optarg; break;
			case 'T': timeline = optarg; break;
			default: usage(); return 1;
		}
	}
	if(p.size < sizeof(Hdr) || p.size > MAX_PKT)
	{
		fprintf(stderr, "--size 范围 %zu~%d\n", sizeof(Hdr), MAX_PKT);
		return 1;
	}
	if(p.duration_ms > 3500 * 1000)
	{
		fprintf(stderr, "--duration 最长 3500 秒\n");
		return 1;
	}
	if(!strcmp(cmd, "server"))
		return cmd_server(port, bind_if);
	if(!strcmp(cmd, "client"))
		return cmd_client(host, port, bind_if, p, trace, timeline);
	if(!strcmp(cmd, "mtu"))
		return cmd_mtu(host, port, bind_if);
	usage();
	return 1;
}
