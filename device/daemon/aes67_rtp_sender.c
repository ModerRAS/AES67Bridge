/*
 * aes67_rtp_sender_v4.c —— root 侧 AES67 发送守护 v4
 * 读 /data/haltee/live.pcm（FIFO 原始 PCM）+ /data/haltee/live.meta（rate/ch/fmt/stream_id）
 * - rate 48000：直接转 L24
 * - rate 44100：用 libsamplerate 0.2.2 best-quality 系数的 160 相 × 287 抽头
 *   多相 FIR（Q30 定点）重采样到 48000（系数经 NativeSpatialPipe sinc_resampler 包装移植；
 *   libsamplerate 表 BSD-2-Clause，wrapper Apache-2.0；见 sinc_poly_160.h）
 * - 输出：L24 / 48000 / 实际声道数(1-8) RTP，PT97，1ms=48 帧，组播 239.69.2.1:5004
 * - 严格 1ms 节拍、固定环形缓冲（无 memmove）、FIFO eager 读、SDP 按通道数写盘
 * 不修改 HAL/vendor；只读 FIFO。
 */
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <sched.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <stdint.h>
#include "sinc_poly_160.h"

#define RTP_GROUP    "239.69.2.1"
#define RTP_PORT     5004
#define RTP_PT       97
#define RTP_FRAMES   48
#define OUT_RATE     48000
#define LOCAL_IP     "192.168.63.158"
#define RING_FRAMES  8000      /* ~166ms @48k */
#define RING_KEEP    4800      /* >100ms 丢弃最旧 */
#define HIST_FRAMES  4096   /* power of two, >= 143+chunk */
#define MAX_CH       8
#define STAT_INTERVAL_MS 10000
#define SEND_HIST_CAP 2048
#define DEFAULT_SDP  "/data/haltee/aes67-v4.sdp"

typedef enum { FMT_S16 = 0, FMT_S24_PACKED, FMT_S24_32, FMT_F32, FMT_UNKNOWN } sample_fmt;

static int32_t g_ring[RING_FRAMES][MAX_CH];
static uint32_t g_ring_w = 0, g_ring_r = 0;
static int32_t g_hist[MAX_CH][HIST_FRAMES];
static uint64_t g_hist_count = 0;
static uint64_t g_pos_num = 0;     /* 输出位置，单位 1/160 输入帧 */
static uint32_t g_send_us[SEND_HIST_CAP];
static uint32_t g_send_n = 0;
static volatile int g_stop = 0;
static unsigned long long g_packets = 0, g_bytes = 0, g_send_errs = 0, g_drops = 0, g_underruns = 0;
static unsigned long long g_read_bytes = 0, g_reads = 0, g_eofs = 0;
static unsigned int g_fifo_last_bytes = 0, g_fifo_max_bytes = 0;
static unsigned long long g_rs_in_frames = 0, g_rs_out_frames = 0;
static int g_sched_prio = 0, g_trace_data_left = 0;
static int g_flushed = 0;
static int32_t g_zero_frame[MAX_CH];
static const char* g_status_path = "/data/haltee/status.json";
static double g_lvl_sumsq[MAX_CH];
static unsigned long long g_lvl_n = 0;
static int g_lvl_ch = 0;
static const char* g_sdp_path = DEFAULT_SDP;

static void on_signal(int s) { (void)s; g_stop = 1; }
static long long now_ns(void) { struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); return (long long)ts.tv_sec*1000000000LL + ts.tv_nsec; }
static void log_line(const char* fmt, ...) { va_list ap; va_start(ap, fmt); vfprintf(stderr, fmt, ap); va_end(ap); fputc('\n', stderr); fflush(stderr); }

static int bytes_per_sample(sample_fmt f) {
    switch (f) { case FMT_S16: return 2; case FMT_S24_PACKED: return 3; case FMT_S24_32: return 4; case FMT_F32: return 4; default: return 2; }
}
static sample_fmt parse_fmt_name(const char* t) {
    if (!t) return FMT_S16;
    if (strstr(t, "PACKED") || strstr(t, "24_PACKED") || strstr(t, "24_BIT_PACKED") || strstr(t, "S24P")) return FMT_S24_PACKED;
    if (strstr(t, "PCM_24") || strstr(t, "S24")) return FMT_S24_32;
    if (strstr(t, "FLOAT") || strstr(t, "F32")) return FMT_F32;
    if (strstr(t, "PCM_16") || strstr(t, "S16")) return FMT_S16;
    return FMT_S16;
}
static int32_t decode_sample(const uint8_t* p, sample_fmt f) {
    switch (f) {
    case FMT_S16: { int16_t v; memcpy(&v, p, 2); return (int32_t)v << 8; }
    case FMT_S24_PACKED: { int32_t v=(int32_t)p[0]|((int32_t)p[1]<<8)|((int32_t)p[2]<<16); if (v&0x800000) v|=0xFF000000; return v; }
    case FMT_S24_32: { int32_t v; memcpy(&v, p, 4); return v >> 8; }
    case FMT_F32: { float v; memcpy(&v, p, 4); double d=(double)v*8388607.0; if (d>8388607.0) d=8388607.0; if (d<-8388608.0) d=-8388608.0; return (int32_t)(d>=0?d+0.5:d-0.5); }
    default: return 0;
    }
}

typedef struct { int source_fd; uint8_t carry[64]; size_t carry_len; sample_fmt fmt; int channels; int rate; long long stream_id; int valid; } meta_state;

static void parse_meta(const char* path, meta_state* m) {
    if (!path) return;
    FILE* f = fopen(path, "r");
    if (!f) return;
    char line[512];
    int ch = m->channels, rate = m->rate; sample_fmt fmt = m->fmt; long long sid = m->stream_id;
    while (fgets(line, sizeof line, f)) {
        char* p;
        if ((p = strstr(line, "stream_id="))) sid = atoll(p + 10);
        else if ((p = strstr(line, "fmt="))) {
            if (p[4]=='0' && (p[5]=='x'||p[5]=='X')) {
                long v = strtol(p+4, NULL, 0);
                switch (v) { case 1: fmt=FMT_S16; break; case 3: case 4: fmt=FMT_S24_32; break; case 5: fmt=FMT_F32; break; case 6: fmt=FMT_S24_PACKED; break; default: break; }
            } else fmt = parse_fmt_name(p+4);
        }
        else if ((p = strstr(line, "fmt_name="))) fmt = parse_fmt_name(p+9);
        else if ((p = strstr(line, "rate="))) rate = atoi(p+5);
        else if ((p = strstr(line, "ch="))) ch = atoi(p+3);
    }
    fclose(f);
    if (ch < 1) ch = 1; if (ch > MAX_CH) ch = MAX_CH;
    if (rate <= 0) rate = 48000;
    m->channels = ch; m->rate = rate; m->fmt = fmt; m->stream_id = sid; m->valid = 1;
}

static void ring_push_frame(const int32_t* frame) {
    uint32_t slot = g_ring_w % RING_FRAMES;
    for (int c = 0; c < MAX_CH; c++) g_ring[slot][c] = c < 8 ? frame[c] : 0;
    g_ring_w++;
    if ((int)(g_ring_w - g_ring_r) > RING_KEEP) {
        g_ring_r += (uint32_t)((g_ring_w - g_ring_r) - RING_KEEP);
        g_drops += (unsigned long long)((g_ring_w - g_ring_r) - RING_KEEP);
    }
}
static int ring_frames(void) { return (int)(g_ring_w - g_ring_r); }
static void ring_pop(int32_t out[][MAX_CH], int frames) {
    for (int i = 0; i < frames; i++) {
        uint32_t slot = (g_ring_r + (uint32_t)i) % RING_FRAMES;
        for (int c = 0; c < MAX_CH; c++) out[i][c] = g_ring[slot][c];
    }
    g_ring_r += (uint32_t)frames;
}
static void hist_push(const int32_t* frame, int ch) {
    /* 输入样本 n 存在 slot=(n+143)&(HIST-1)：这样输出窗口 i-143..i+143 映射到 slot (i..i+286)&mask，
       且前 143 个 slot 的零值是负 index 的零填充 */
    uint32_t slot = (uint32_t)((g_hist_count + 143u) & (HIST_FRAMES - 1));
    for (int c = 0; c < ch; c++) g_hist[c][slot] = frame[c];
    g_hist_count++;
}

static int open_socket(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) return -1;
    int one = 1; setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct in_addr ifa; if (inet_pton(AF_INET, LOCAL_IP, &ifa) == 1) setsockopt(fd, IPPROTO_IP, IP_MULTICAST_IF, &ifa, sizeof ifa);
    unsigned char ttl = 32; setsockopt(fd, IPPROTO_IP, IP_MULTICAST_TTL, &ttl, sizeof ttl);
    unsigned char loop = 0; setsockopt(fd, IPPROTO_IP, IP_MULTICAST_LOOP, &loop, sizeof loop);
    int sndbuf = 4 << 20; setsockopt(fd, SOL_SOCKET, SO_SNDBUF, &sndbuf, sizeof sndbuf);
    return fd;
}

static void write_sdp(int channels, int in_rate) {
    char sdp[900];
    int n = snprintf(sdp, sizeof sdp,
        "v=0\r\n"
        "o=- %u 0 IN IP4 " LOCAL_IP "\r\n"
        "s=AES67 Z9X HAL tee (v4 SRC)\r\n"
        "i=source_rate=%d source_channels=%d\r\n"
        "c=IN IP4 " RTP_GROUP "/32\r\n"
        "t=0 0\r\n"
        "m=audio %d RTP/AVP %d\r\n"
        "a=rtpmap:%d L24/48000/%d\r\n"
        "a=ptime:1\r\n"
        "a=sendonly\r\n"
        "a=ts-refclk:local\r\n"
        "a=mediaclk:direct=0\r\n",
        (unsigned)((unsigned long)time(0) & 0x7ffffffful), in_rate, channels,
        RTP_PORT, RTP_PT, RTP_PT, channels);
    if (n <= 0) return;
    FILE* f = fopen(g_sdp_path, "w");
    if (!f) { log_line("SDP open failed path=%s errno=%d", g_sdp_path, errno); return; }
    fwrite(sdp, 1, (size_t)n, f); fclose(f);
    log_line("SDP written %s ch=%d in_rate=%d", g_sdp_path, channels, in_rate);
}

static uint32_t send_us_percentile(double p) {
    if (g_send_n == 0) return 0;
    static uint32_t copy[SEND_HIST_CAP]; int n = (int)g_send_n;
    memcpy(copy, g_send_us, sizeof(uint32_t) * n);
    for (int i = 1; i < n; i++) { uint32_t v = copy[i]; int j=i-1; while (j>=0 && copy[j]>v){copy[j+1]=copy[j];j--;} copy[j+1]=v; }
    int idx = (int)(p*n); if (idx>=n) idx=n-1; return copy[idx];
}
/* 输入结束时把重采样器尾部 flush 出来：补 143 个零帧继续产生输出，直到覆盖所有真实输入样本 */
static void flush_resampler(int ch) {
    if (g_flushed || g_hist_count == 0) return;
    g_flushed = 1;
    for (int i = 0; i < 143; i++) hist_push(g_zero_frame, ch);
    while (g_hist_count > (g_pos_num / SINC_PHASES) + 143) {
        int64_t base = (int64_t)(g_pos_num / SINC_PHASES);
        int phase = (int)(g_pos_num % SINC_PHASES);
        int32_t frame[MAX_CH];
        const int32_t* qrow = kSincPolyQ30[phase];
        uint32_t slot0 = (uint32_t)base & (HIST_FRAMES - 1);
        for (int c = 0; c < ch; c++) {
            int64_t sum = 0;
            const int32_t* h = g_hist[c];
            for (int t = 0; t < SINC_TAPS; t++) {
                sum += (int64_t)h[(slot0 + (uint32_t)t) & (HIST_FRAMES - 1)] * (int64_t)qrow[t];
            }
            int64_t v = sum >> 30;
            if (v > 8388607) v = 8388607; if (v < -8388608) v = -8388608;
            frame[c] = (int32_t)v;
        }
        int32_t f8[MAX_CH]; memcpy(f8, frame, sizeof f8); ring_push_frame(f8);
        g_pos_num += 147;
        g_rs_out_frames++;
    }
}

/* 每秒写 /data/haltee/status.json（0644），供 AES67 控制台 App 读取；失败忽略 */
static void write_status(int ch, int rate, int fmt, long long sid, uint32_t ssrc, uint16_t seq)
{
    char buf[4096];
    int n = 0;
    struct timespec rts; clock_gettime(CLOCK_REALTIME, &rts);
    long long ts_ms = (long long)rts.tv_sec * 1000LL + rts.tv_nsec / 1000000LL;
    int lowlat = (access("/data/haltee/latency.frames", F_OK) == 0);
    const char* fmt_name = (fmt == (int)FMT_S16) ? "PCM_16" :
                           (fmt == (int)FMT_S24_PACKED) ? "PCM_24_PACKED" :
                           (fmt == (int)FMT_S24_32) ? "PCM_24" :
                           (fmt == (int)FMT_F32) ? "FLOAT" : "PCM_16";
    n += snprintf(buf + n, sizeof buf - n,
        "{\"ts\":%lld,\"mode\":\"%s\",\"hal\":\"running\","
        "\"stream\":{\"id\":%lld,\"fmt\":\"%s\",\"rate\":%d,\"ch\":%d,\"codec\":\"pcm\"},"
        "\"rtp\":{\"packets\":%llu,\"send_err\":%llu,\"ssrc\":\"0x%08x\",\"pt\":%d,\"dest\":\"239.69.2.1:5004\",\"ch\":%d},"
        "\"src\":{\"rs_in\":%llu,\"rs_out\":%llu},\"levels\":[",
        ts_ms, lowlat ? "lowlat" : "normal", sid, fmt_name, rate, ch,
        g_packets, g_send_errs, ssrc, RTP_PT, ch,
        g_rs_in_frames, g_rs_out_frames);
    for (int c = 0; c < ch && c < MAX_CH; c++) {
        double dbfs = -120.0;
        if (g_lvl_n > 0 && g_lvl_sumsq[c] > 0.0) {
            double rms = sqrt(g_lvl_sumsq[c] / (double)g_lvl_n);
            dbfs = 20.0 * log10(rms / 8388608.0);
            if (dbfs < -120.0) dbfs = -120.0;
            if (dbfs > 0.0) dbfs = 0.0;
        }
        n += snprintf(buf + n, sizeof buf - n, "%s%.2f", c ? "," : "", dbfs);
    }
    n += snprintf(buf + n, sizeof buf - n,
        "],\"ring\":{\"fill\":%d},\"fifo\":{\"bytes\":%u}}\n",
        ring_frames(), g_fifo_last_bytes);
    char tmp[256];
    snprintf(tmp, sizeof tmp, "%s.tmp", g_status_path);
    FILE* fp = fopen(tmp, "w");
    if (fp) {
        fwrite(buf, 1, (size_t)n, fp);
        fclose(fp);
        chmod(tmp, 0644);
        if (rename(tmp, g_status_path) != 0) unlink(tmp);
        chmod(g_status_path, 0644);
    }
    for (int c = 0; c < MAX_CH; c++) g_lvl_sumsq[c] = 0.0;
    g_lvl_n = 0;
    g_lvl_ch = ch;
    (void)seq;
}

static void sleep_until(long long due) {
    long long n = now_ns(); if (due <= n) return;
    struct timespec ts; ts.tv_sec=(time_t)((due-200000)/1000000000LL); ts.tv_nsec=(long)((due-200000)%1000000000LL);
    if (due - n > 500000) clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL);
    while (now_ns() < due) { }
}

int main(int argc, char** argv) {
    const char* fifo_path = "/data/haltee/live.pcm";
    const char* meta_path = "/data/haltee/live.meta";
    double duration = 0;
    int force_ch = 0, force_rate = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--fifo") && i+1 < argc) fifo_path = argv[++i];
        else if (!strcmp(argv[i], "--meta") && i+1 < argc) meta_path = argv[++i];
        else if (!strcmp(argv[i], "--seconds") && i+1 < argc) duration = atof(argv[++i]);
        else if (!strcmp(argv[i], "--ch") && i+1 < argc) force_ch = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--rate") && i+1 < argc) force_rate = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--sched-fifo") && i+1 < argc) g_sched_prio = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--trace-data") && i+1 < argc) g_trace_data_left = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--sdp") && i+1 < argc) g_sdp_path = argv[++i];
        else if (!strcmp(argv[i], "--status") && i+1 < argc) g_status_path = argv[++i];
        else if (!strcmp(argv[i], "--help")) {
            printf("v4.3 usage: %s --fifo F --meta M [--seconds N] [--sched-fifo PRIO] [--sdp PATH] [--status PATH] [--ch N] [--rate R]\n", argv[0]);
            return 0;
        }
    }
    struct sigaction sa; memset(&sa,0,sizeof sa); sa.sa_handler=on_signal; sigemptyset(&sa.sa_mask); sa.sa_flags=0;
    sigaction(SIGINT,&sa,NULL); sigaction(SIGTERM,&sa,NULL);
    if (g_sched_prio > 0) {
        struct sched_param sp; memset(&sp,0,sizeof sp); sp.sched_priority=g_sched_prio;
        int rc = sched_setscheduler(0, SCHED_FIFO, &sp);
        log_line("SCHED_FIFO prio=%d rc=%d errno=%d", g_sched_prio, rc, errno);
    }
    int fifo_fd = open(fifo_path, O_RDONLY | O_NONBLOCK);
    if (fifo_fd < 0) { log_line("open fifo %s failed errno=%d", fifo_path, errno); return 2; }
    int fd = open_socket(); if (fd < 0) { log_line("socket failed errno=%d", errno); return 2; }
    struct sockaddr_in dst; memset(&dst,0,sizeof dst); dst.sin_family=AF_INET; dst.sin_port=htons(RTP_PORT); inet_pton(AF_INET, RTP_GROUP, &dst.sin_addr);

    meta_state meta; memset(&meta,0,sizeof meta); meta.fmt=FMT_S16; meta.channels=2; meta.rate=OUT_RATE;
    int cur_ch = 2, cur_rate = OUT_RATE, cur_fmt = FMT_S16; long long cur_sid = -1;
    uint16_t seq = 0; uint32_t ts = 0; uint32_t ssrc = 0x4E535034u;
    int first_sent = 0, pending_sdp = 1;
    long long start_ns = now_ns(), next_due = start_ns, last_stat = start_ns;
    uint8_t tmp[65536];
    int32_t frame[MAX_CH];
    log_line("v4.3 start fifo=%s meta=%s sdp=%s status=%s seconds=%.0f", fifo_path, meta_path, g_sdp_path, g_status_path, duration);
    while (!g_stop) {
        long long now = now_ns();
        if (duration > 0 && (now - start_ns) / 1e9 >= duration) break;
        /* --- meta 变化（250ms 节流，避免每轮读文件） --- */
        static long long last_meta_ns = 0;
        if (now_ns() - last_meta_ns > 250000000LL) { parse_meta(meta_path, &meta); last_meta_ns = now_ns(); }
        int ch = force_ch > 0 ? force_ch : meta.channels;
        int rate = force_rate > 0 ? force_rate : meta.rate;
        if (ch < 1) ch = 1; if (ch > MAX_CH) ch = MAX_CH;
        if (ch != cur_ch || rate != cur_rate || (int)meta.fmt != cur_fmt || meta.stream_id != cur_sid) {
            /* 重置状态（环形缓冲/重采样历史）；SDP 重建 */
            cur_ch = ch; cur_rate = rate; cur_fmt = (int)meta.fmt; cur_sid = meta.stream_id;
            g_ring_w = g_ring_r = 0; g_hist_count = 0; g_pos_num = 0; g_drops = 0; g_flushed = 0;
            pending_sdp = 1;
            ssrc = 0x4E535034u ^ (uint32_t)now_ns();
            seq = (uint16_t)(ssrc >> 8); ts = (uint32_t)(now_ns() / 1000);
            log_line("META ch=%d rate=%d fmt=%d sid=%lld", cur_ch, cur_rate, cur_fmt, cur_sid);
        }
        if (pending_sdp) { write_sdp(cur_ch, cur_rate); pending_sdp = 0; }
        if (cur_rate != 48000 && cur_rate != 44100) { usleep(100000); continue; }
        /* --- eager 读 FIFO --- */
        int avail = 0; ioctl(fifo_fd, FIONREAD, &avail);
        size_t total = 0;
        if (avail > 0) {
            g_fifo_last_bytes = (unsigned)avail; if ((unsigned)avail > g_fifo_max_bytes) g_fifo_max_bytes = (unsigned)avail;
            for (;;) {
                ssize_t rd = read(fifo_fd, tmp + total, sizeof(tmp) - total);
                if (rd > 0) { total += (size_t)rd; if (total == sizeof(tmp)) break; }
                else if (rd == 0) { g_eofs++; flush_resampler(cur_ch); usleep(50000); break; }
                else break;
            }
            if (total > 0) { g_read_bytes += total; g_reads++; }
        }
        size_t frame_bytes = (size_t)bytes_per_sample((sample_fmt)cur_fmt) * (size_t)cur_ch;
        size_t off = 0;
        if (total > 0) {
            /* 拼接 carry */
            while (off < total) {
                size_t need = frame_bytes - meta.carry_len;
                size_t take = total - off; if (take > need) take = need;
                memcpy(meta.carry + meta.carry_len, tmp + off, take);
                meta.carry_len += take; off += take;
                if (meta.carry_len < frame_bytes) break;
                for (int c = 0; c < cur_ch; c++) frame[c] = decode_sample(meta.carry + (size_t)c * bytes_per_sample((sample_fmt)cur_fmt), (sample_fmt)cur_fmt);
                meta.carry_len = 0;
                if (cur_rate == 48000) {
                    int32_t f8[MAX_CH]; memcpy(f8, frame, sizeof f8); ring_push_frame(f8);
                } else {
                    hist_push(frame, cur_ch); g_rs_in_frames++;
                    /* 产生所有可输出的 48k 帧 */
                    while (g_hist_count > (g_pos_num / SINC_PHASES) + 143) {
                        int64_t base = (int64_t)(g_pos_num / SINC_PHASES);
                        int phase = (int)(g_pos_num % SINC_PHASES);
                        {
                            const int32_t* qrow = kSincPolyQ30[phase];
                            uint32_t slot0 = (uint32_t)base & (HIST_FRAMES - 1);
                            for (int c = 0; c < cur_ch; c++) {
                                int64_t sum = 0;
                                const int32_t* h = g_hist[c];
                                for (int t = 0; t < SINC_TAPS; t++) {
                                    sum += (int64_t)h[(slot0 + (uint32_t)t) & (HIST_FRAMES - 1)] * (int64_t)qrow[t];
                                }
                                int64_t v = sum >> 30;
                                if (v > 8388607) v = 8388607; if (v < -8388608) v = -8388608;
                                frame[c] = (int32_t)v;
                            }
                        }
                        int32_t f8[MAX_CH]; memcpy(f8, frame, sizeof f8); ring_push_frame(f8);
                        g_pos_num += (147 * 1);   /* step 44100/48000 = 147/160 */
                        g_rs_out_frames++;
                    }
                }
            }
        }
        /* --- 严格 1ms 发包 --- */
        if (ring_frames() >= RTP_FRAMES) {
            if (next_due > now_ns()) sleep_until(next_due);
            long long t0 = now_ns();
            int payload_len = RTP_FRAMES * cur_ch * 3;
            uint8_t pkt[12 + RTP_FRAMES * MAX_CH * 3];
            pkt[0]=0x80; pkt[1]=RTP_PT;
            pkt[2]=(uint8_t)(seq>>8); pkt[3]=(uint8_t)seq;
            pkt[4]=(uint8_t)(ts>>24); pkt[5]=(uint8_t)(ts>>16); pkt[6]=(uint8_t)(ts>>8); pkt[7]=(uint8_t)ts;
            pkt[8]=(uint8_t)(ssrc>>24); pkt[9]=(uint8_t)(ssrc>>16); pkt[10]=(uint8_t)(ssrc>>8); pkt[11]=(uint8_t)ssrc;
            int32_t fr[RTP_FRAMES][MAX_CH]; ring_pop(fr, RTP_FRAMES);
            uint8_t* q = pkt + 12;
            for (int i = 0; i < RTP_FRAMES; i++) for (int c = 0; c < cur_ch; c++) {
                uint32_t v = (uint32_t)fr[i][c]; *q++=(uint8_t)(v>>16); *q++=(uint8_t)(v>>8); *q++=(uint8_t)v;
            }
            for (int i = 0; i < RTP_FRAMES; i++) for (int c = 0; c < cur_ch && c < MAX_CH; c++) {
                double sv = (double)fr[i][c];
                g_lvl_sumsq[c] += sv * sv;
            }
            g_lvl_n += (unsigned long long)RTP_FRAMES;
            g_lvl_ch = cur_ch;
            ssize_t sent = sendto(fd, pkt, (size_t)(12+payload_len), MSG_DONTWAIT|MSG_NOSIGNAL, (const struct sockaddr*)&dst, sizeof dst);
            long long t1 = now_ns();
            g_send_us[g_send_n % SEND_HIST_CAP] = (uint32_t)((t1-t0)/1000); if (g_send_n < SEND_HIST_CAP) g_send_n++;
            if (sent < 0) { if (g_send_errs<3) log_line("SEND ERR errno=%d (%s) seq=%u", errno, strerror(errno), (unsigned)seq); g_send_errs++; }
            else { g_packets++; g_bytes += (unsigned long long)sent; if (!first_sent) { first_sent=1; log_line("FIRST packet seq=%u ts=%u ssrc=0x%08x ch=%d pt=%d payload=%dB", (unsigned)seq,(unsigned)ts,ssrc,cur_ch,RTP_PT,payload_len); } }
            seq++; ts += RTP_FRAMES;
            next_due += 1000000LL;
            if (next_due < t1 - 5000000LL) next_due = t1;
        } else {
            struct pollfd pfd; pfd.fd=fifo_fd; pfd.events=POLLIN; pfd.revents=0;
            long long wait_us = (next_due - now_ns())/1000; if (wait_us <= 0 || wait_us > 1000) wait_us = 1000;
            poll(&pfd, 1, (int)wait_us);
            if ((pfd.revents & POLLHUP) && !(pfd.revents & POLLIN)) {
                /* 写端关闭（live off / 流结束）：flush 重采样器尾部 */
                flush_resampler(cur_ch);
            }
        }
        {
            static long long last_status_ns = 0;
            if (now_ns() - last_status_ns > 1000000000LL) {
                last_status_ns = now_ns();
                write_status(cur_ch, cur_rate, cur_fmt, cur_sid, ssrc, seq);
            }
        }
        if (now_ns() - last_stat > STAT_INTERVAL_MS*1000000LL) {
            last_stat = now_ns();
            log_line("STAT packets=%llu bytes=%llu send_err=%llu drops=%llu read_bytes=%llu reads=%llu eofs=%llu ring_fill=%d fifo_bytes=%u fifo_max=%u send_us_p50=%u p99=%u max=%u ch=%d rate=%d rs_in=%llu rs_out=%llu",
                g_packets,g_bytes,g_send_errs,g_drops,g_read_bytes,g_reads,g_eofs,ring_frames(),g_fifo_last_bytes,g_fifo_max_bytes,
                send_us_percentile(0.5), send_us_percentile(0.99), send_us_percentile(1.0), cur_ch, cur_rate, g_rs_in_frames, g_rs_out_frames);
        }
    }
    /* 退出前 flush 重采样尾部并尽量把环里剩余包发完（不按 1ms 等，只用于收尾） */
    flush_resampler(cur_ch);
    {
        int32_t fr[RTP_FRAMES][MAX_CH];
        while (ring_frames() >= RTP_FRAMES) {
            int payload_len = RTP_FRAMES * cur_ch * 3;
            uint8_t pkt[12 + RTP_FRAMES * MAX_CH * 3];
            pkt[0]=0x80; pkt[1]=RTP_PT;
            pkt[2]=(uint8_t)(seq>>8); pkt[3]=(uint8_t)seq;
            pkt[4]=(uint8_t)(ts>>24); pkt[5]=(uint8_t)(ts>>16); pkt[6]=(uint8_t)(ts>>8); pkt[7]=(uint8_t)ts;
            pkt[8]=(uint8_t)(ssrc>>24); pkt[9]=(uint8_t)(ssrc>>16); pkt[10]=(uint8_t)(ssrc>>8); pkt[11]=(uint8_t)ssrc;
            ring_pop(fr, RTP_FRAMES);
            uint8_t* q = pkt + 12;
            for (int i = 0; i < RTP_FRAMES; i++) for (int c = 0; c < cur_ch; c++) {
                uint32_t v = (uint32_t)fr[i][c]; *q++=(uint8_t)(v>>16); *q++=(uint8_t)(v>>8); *q++=(uint8_t)v;
            }
            ssize_t sent = sendto(fd, pkt, (size_t)(12+payload_len), MSG_DONTWAIT|MSG_NOSIGNAL, (const struct sockaddr*)&dst, sizeof dst);
            if (sent > 0) { g_packets++; g_bytes += (unsigned long long)sent; }
            seq++; ts += RTP_FRAMES;
        }
    }
    log_line("EXIT packets=%llu bytes=%llu send_err=%llu drops=%llu read_bytes=%llu reads=%llu eofs=%llu rs_in=%llu rs_out=%llu",
             g_packets,g_bytes,g_send_errs,g_drops,g_read_bytes,g_reads,g_eofs,g_rs_in_frames,g_rs_out_frames);
    close(fd); close(fifo_fd);
    return 0;
}
