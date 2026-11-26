/*
 * haltee.c - transparent audio tee proxy for /vendor/lib/hw/audio.primary.amlogic.so
 *
 * PoC-1: loaded by hw_get_module() inside vendor.audio-hal (via overlayfs upper),
 * dlopens the original library (audio.primary.amlogic.so.orig), then interposes
 *   - audio_hw_device_t.open_output_stream   (Android 11 ABI offset 60 on 32-bit)
 *   - audio_stream_out_t.write               (Android 11 ABI offset 64 on 32-bit)
 * logs format/sample-rate/channel/bytes + first bytes + PCM level + bitstream sync
 * words, and dumps raw payload to /data/aes67bridge/dump/.  Everything is transparent:
 * the original function is always called, and its return value is passed through.
 *
 * Nothing is persisted; unmounting the overlay + restarting the HAL restores stock.
 *
 * Build (NDK 27):
 *   armv7a-linux-androideabi30-clang -shared -fPIC -O2 -Wall -o audio.primary.amlogic.so haltee.c symmap.c -lm
 */
#define _GNU_SOURCE 1
#include <stdint.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <dlfcn.h>
#include <pthread.h>
#include <time.h>
#include <errno.h>
#include <math.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <link.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/stat.h>
#include <pthread.h>

#define HALTEE_ROOT  "/data/aes67bridge"
#define HALTEE_LOGDIR HALTEE_ROOT "/logs"
#define HALTEE_LOG   HALTEE_ROOT "/logs/haltee.log"
#define HALTEE_DUMP  HALTEE_ROOT "/dump"
#define ORIG_PATH    "/vendor/lib/hw/audio.primary.amlogic.so.orig"

#define HARDWARE_MODULE_TAG 0x48574D54u
#define HARDWARE_DEVICE_TAG 0x48574454u

/* Android 11 hardware/libhardware/include/hardware/audio.h, 32-bit */
#define DEV_OFF_OPEN_OUTPUT_STREAM 60u
#define STREAM_OUT_OFF_WRITE       64u

#define MAX_STREAMS   24
#define DUMP_ENABLE_FILE "/data/aes67bridge/dump.enable"
#define DUMP_CAP          (2u * 1024u * 1024u)   /* 2 MB per stream, only when dump.enable exists */
#define LOGBUF        1400

/* ---------------- minimal HAL ABI ---------------- */
typedef struct hw_module_t hw_module_t;
typedef struct hw_module_methods_t hw_module_methods_t;
typedef struct hw_device_t hw_device_t;

struct hw_module_methods_t {
    int (*open)(const hw_module_t* module, const char* id, hw_device_t** device);
};
struct hw_module_t {
    uint32_t tag;
    uint16_t module_api_version;
    uint16_t hal_api_version;
    const char* id;
    const char* name;
    const char* author;
    hw_module_methods_t* methods;
    void* dso;
    uint32_t reserved[32 - 7];
};
struct hw_device_t {
    uint32_t tag;
    uint32_t version;
    hw_module_t* module;
    int (*close)(hw_device_t* device);
};
struct audio_module { hw_module_t common; };

/* struct audio_config: only the first three fields are read */
struct haltee_config {
    uint32_t sample_rate;
    uint32_t channel_mask;
    uint32_t format;
    uint8_t  rest[64];
};

typedef int (*open_out_fn)(void* dev, uint32_t handle, uint32_t devices,
                           uint32_t flags, struct haltee_config* cfg,
                           void** stream_out, const char* address);
typedef ssize_t (*stream_write_fn)(void* stream, const void* buffer, size_t bytes);

/* ---------------- symbol map (auto-generated) ---------------- */
struct haltee_sym { uint32_t addr; const char* name; };
extern const struct haltee_sym haltee_syms[];
extern const unsigned int haltee_syms_count;

/* ---------------- globals ---------------- */
static uintptr_t g_base = 0;
static uintptr_t g_base_end = 0;
static void* g_orig_handle = 0;
static open_out_fn g_orig_open_out = 0;
static int g_open_out_off = -1;
static int g_open_out_patched = 0;
static int g_logfd = -1;
static pthread_mutex_t g_loglk = PTHREAD_MUTEX_INITIALIZER;
static int (*g_log_print)(int prio, const char* tag, const char* fmt, ...) = 0;
static int g_unknown_writes = 0;
static int g_init_done = 0;

typedef struct {
    int used;
    int idx;
    void* stream;
    stream_write_fn orig_write;
    int write_off;
    uint32_t handle;
    uint32_t devices;
    uint32_t flags;
    uint32_t format;
    uint32_t rate;
    uint32_t chmask;
    char address[64];
    uint64_t writes;
    uint64_t bytes;
    uint64_t cap;
    uint64_t dump_bytes;
    int dump_fd;
    char dump_path[160];
    uint64_t last_summary_ms;
} haltee_stream;

static haltee_stream g_streams[MAX_STREAMS];
static int g_stream_seq = 0;

/* ---------------- misc helpers ---------------- */
static uint64_t haltee_now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)(ts.tv_nsec / 1000000);
}

#define MAX_MAPS 4096
struct haltee_map { uintptr_t start, end; int r, w; };
static struct haltee_map g_maps[MAX_MAPS];
static int g_maps_n = -1;

static void haltee_load_maps(void)
{
    g_maps_n = 0;
    FILE* f = fopen("/proc/self/maps", "r");
    if (!f) return;
    char line[512];
    while (g_maps_n < MAX_MAPS && fgets(line, sizeof line, f)) {
        unsigned long s = 0, e = 0;
        char perms[8];
        memset(perms, 0, sizeof perms);
        if (sscanf(line, "%lx-%lx %7s", &s, &e, perms) == 3 && e > s) {
            g_maps[g_maps_n].start = (uintptr_t)s;
            g_maps[g_maps_n].end = (uintptr_t)e;
            g_maps[g_maps_n].r = (perms[0] == 'r');
            g_maps[g_maps_n].w = (perms[1] == 'w');
            g_maps_n++;
        }
    }
    fclose(f);
}

static const struct haltee_map* haltee_map_for(uintptr_t a)
{
    if (g_maps_n < 0) haltee_load_maps();
    for (int i = 0; i < g_maps_n; i++) {
        if (a >= g_maps[i].start && a < g_maps[i].end) return &g_maps[i];
    }
    return 0;
}

static void haltee_refresh_maps(void)
{
    g_maps_n = -1;
    haltee_load_maps();
}

static uint32_t haltee_read32(uintptr_t a)
{
    const struct haltee_map* m = haltee_map_for(a);
    if (!m || !m->r) return 0;
    return *(volatile uint32_t*)a;
}

static int haltee_is_code_ptr(uint32_t v)
{
    if (!g_base) return 0;
    uintptr_t p = (uintptr_t)v;
    return p >= g_base && p < g_base_end;
}

static int haltee_patch32(uintptr_t addr, uint32_t val)
{
    const struct haltee_map* m = haltee_map_for(addr);
    if (!m || !m->r) return -1;
    if (m->w) {
        *(volatile uint32_t*)addr = val;
        return 0;
    }
    uintptr_t pg = addr & ~(uintptr_t)0xfff;
    uintptr_t last = (addr + 4u) & ~(uintptr_t)0xfff;
    size_t len = (size_t)(last - pg) + 0x1000u;
    if (mprotect((void*)pg, len, PROT_READ | PROT_WRITE) != 0) return -2;
    *(volatile uint32_t*)addr = val;
    mprotect((void*)pg, len, PROT_READ);
    return 0;
}

const char* haltee_sym_name(uint32_t v);
const char* haltee_sym_name(uint32_t v)
{
    if (!v || !g_base) return 0;
    uint32_t off = v - (uint32_t)g_base;
    if (v < g_base || v >= g_base_end) return 0;
    int lo = 0, hi = (int)haltee_syms_count - 1, best = -1;
    while (lo <= hi) {
        int mid = (lo + hi) / 2;
        if (haltee_syms[mid].addr == off) return haltee_syms[mid].name;
        if (haltee_syms[mid].addr < off) { best = mid; lo = mid + 1; }
        else hi = mid - 1;
    }
    (void)best;
    return 0;
}

static const char* haltee_sym_near(uint32_t v)
{
    if (!v || !g_base) return 0;
    uint32_t off = v - (uint32_t)g_base;
    if (v < g_base || v >= g_base_end) return 0;
    int lo = 0, hi = (int)haltee_syms_count - 1, best = -1;
    while (lo <= hi) {
        int mid = (lo + hi) / 2;
        if (haltee_syms[mid].addr <= off) { best = mid; lo = mid + 1; }
        else hi = mid - 1;
    }
    if (best < 0) return 0;
    if (off - haltee_syms[best].addr < 0x2000u) return haltee_syms[best].name;
    return 0;
}

/* ---------------- logging ---------------- */
#define LOG_FILE_CAP (2u * 1024u * 1024u)   /* 2 MB then rotate once */
static unsigned long long g_log_bytes = 0;

static void haltee_logv(const char* fmt, va_list ap);

static void haltee_log(const char* fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    haltee_logv(fmt, ap);
    va_end(ap);
}

static void haltee_logv(const char* fmt, va_list ap)
{
    char msg[LOGBUF];
    vsnprintf(msg, sizeof msg, fmt, ap);
    if (g_log_print) g_log_print(4 /*INFO*/, "HALTEE", "%s", msg);
    struct timespec rt;
    clock_gettime(CLOCK_REALTIME, &rt);
    char line[LOGBUF + 128];
    int n = snprintf(line, sizeof line, "%lld.%03ld %s\n",
                     (long long)rt.tv_sec, (long)rt.tv_nsec / 1000000, msg);
    if (n <= 0) return;
    if (n > (int)sizeof line - 1) n = (int)sizeof line - 1;
    pthread_mutex_lock(&g_loglk);
    if (g_logfd < 0) {
        mkdir(HALTEE_ROOT, 0775);
        mkdir(HALTEE_LOGDIR, 0700);
        g_logfd = open(HALTEE_LOG, O_WRONLY | O_CREAT | O_APPEND, 0640);
        g_log_bytes = 0;
    }
    if (g_logfd >= 0 && g_log_bytes + (unsigned long long)n > (unsigned long long)LOG_FILE_CAP) {
        close(g_logfd);
        g_logfd = -1;
        rename(HALTEE_LOG, HALTEE_LOG ".1");
        g_logfd = open(HALTEE_LOG, O_WRONLY | O_CREAT | O_TRUNC, 0640);
        g_log_bytes = 0;
    }
    if (g_logfd >= 0) {
        ssize_t r = write(g_logfd, line, (size_t)n);
        if (r > 0) g_log_bytes += (unsigned long long)r;
    }
    pthread_mutex_unlock(&g_loglk);
}

static const char* haltee_fmt_name(uint32_t f)
{
    uint32_t base = f & 0xFF000000u;
    uint32_t sub = f & 0xFFu;
    if (base == 0) {
        switch (sub) {
            case 0: return "PCM_16_BIT(default0)";
            case 1: return "PCM_16_BIT";
            case 2: return "PCM_8_BIT";
            case 3: return "PCM_32_BIT";
            case 4: return "PCM_8_24_BIT";
            case 5: return "PCM_FLOAT";
            case 6: return "PCM_24_BIT_PACKED";
            default: break;
        }
        return "PCM_?";
    }
    switch (base) {
        case 0x01000000u: return "MP3";
        case 0x02000000u: return "AMR_NB";
        case 0x03000000u: return "AMR_WB";
        case 0x04000000u: return "AAC";
        case 0x05000000u: return "HE_AAC";
        case 0x06000000u: return "AAC_ELD";
        case 0x07000000u: return "VORBIS";
        case 0x08000000u: return "OPUS";
        /* vendor (Amlogic) mapping measured on Z9X 8K */
        case 0x09000000u: return "AC3";
        case 0x0A000000u: return "E_AC3";
        case 0x0B000000u: return "DTS";
        case 0x0C000000u: return "DTS_HD";
        case 0x0E000000u: return "IEC61937";
        case 0x0F000000u: return "DOLBY_TRUEHD";
        default: break;
    }
    return "UNKNOWN";
}

static void haltee_flags_str(uint32_t f, char* out, size_t outsz)
{
    static const struct { uint32_t v; const char* n; } fl[] = {
        { 0x00000001u, "DIRECT" }, { 0x00000002u, "PRIMARY" },
        { 0x00000004u, "FAST" }, { 0x00000008u, "DEEP_BUFFER" },
        { 0x00000010u, "COMPRESS_OFFLOAD" }, { 0x00000020u, "NON_BLOCKING" },
        { 0x00000040u, "HW_AV_SYNC" }, { 0x00000080u, "RAW" },
        { 0x00000200u, "MMAP_NOIRQ" }, { 0x00000400u, "VOIP" },
        { 0x00000800u, "DIRECT_MMAP" }, { 0x00001000u, "FAST_MMAP" },
        { 0x00002000u, "SPATIALIZER" }, { 0x00004000u, "MMAP_NOIRQ2" },
    };
    size_t o = 0;
    out[0] = 0;
    for (size_t i = 0; i < sizeof fl / sizeof fl[0]; i++) {
        if (f & fl[i].v) {
            int r = snprintf(out + o, outsz - o, "%s%s", o ? "|" : "", fl[i].n);
            if (r > 0) o += (size_t)r;
            if (o + 32 >= outsz) break;
        }
    }
    if (!o) snprintf(out, outsz, "0");
}

static void haltee_hex(const void* buffer, size_t bytes, char* out, size_t outsz)
{
    const uint8_t* p = (const uint8_t*)buffer;
    size_t n = bytes < 32 ? bytes : 32;
    size_t o = 0;
    out[0] = 0;
    for (size_t i = 0; i < n && o + 4 < outsz; i++) o += (size_t)snprintf(out + o, outsz - o, "%02x", p[i]);
}

/* PCM level statistics; format 1=s16, 3=s32, 5=float */
static void haltee_pcm_stats(uint32_t fmt, const void* buffer, size_t bytes, char* out, size_t outsz)
{
    out[0] = 0;
    if (!buffer || bytes < 4) { snprintf(out, outsz, "stats=n/a"); return; }
    const uint8_t* p = (const uint8_t*)buffer;
    long nz = 0, mn = 0, mx = 0;
    size_t n = 0;
    double sumsq = 0.0;
    if (fmt == 1 || fmt == 0) {
        size_t cnt = bytes / 2;
        if (cnt > 4096) cnt = 4096;
        const int16_t* s = (const int16_t*)p;
        for (size_t i = 0; i < cnt; i++) {
            long v = s[i];
            if (v) nz++;
            if (v < mn) mn = v;
            if (v > mx) mx = v;
            sumsq += (double)v * (double)v;
        }
        n = cnt;
    } else if (fmt == 3) {
        size_t cnt = bytes / 4;
        if (cnt > 2048) cnt = 2048;
        const int32_t* s = (const int32_t*)p;
        for (size_t i = 0; i < cnt; i++) {
            long v = s[i] >> 16;
            if (v) nz++;
            if (v < mn) mn = v;
            if (v > mx) mx = v;
            sumsq += (double)v * (double)v;
        }
        n = cnt;
    } else if (fmt == 5) {
        size_t cnt = bytes / 4;
        if (cnt > 2048) cnt = 2048;
        const float* s = (const float*)p;
        for (size_t i = 0; i < cnt; i++) {
            double v = (double)s[i] * 32768.0;
            long iv = (long)v;
            if (iv) nz++;
            if (iv < mn) mn = iv;
            if (iv > mx) mx = iv;
            sumsq += v * v;
        }
        n = cnt;
    } else {
        snprintf(out, outsz, "stats=nonpcm");
        return;
    }
    double rms = n ? sqrt(sumsq / (double)n) : 0.0;
    snprintf(out, outsz, "nz=%ld/%zu min=%ld max=%ld rms=%.0f%s",
             nz, n, mn, mx, rms, (nz == 0) ? " SILENCE" : "");
}

static void haltee_find_sync(const void* buffer, size_t bytes, char* out, size_t outsz)
{
    static const struct { const char* name; uint8_t sig[4]; int len; } sy[] = {
        { "AC3/EAC3",      { 0x0b, 0x77, 0, 0 }, 2 },
        { "DTS",           { 0x7f, 0xfe, 0x80, 0x01 }, 4 },
        { "TrueHD-major",  { 0xf8, 0x72, 0x6f, 0xba }, 4 },
        { "MAT/MLP-b",     { 0xf8, 0x72, 0x6f, 0xbb }, 4 },
        { "IEC61937-Pa",   { 0x72, 0xf8, 0, 0 }, 2 },
        { "IEC61937-Pb",   { 0x1f, 0x4e, 0, 0 }, 2 },
        { "AC4",           { 0xac, 0x40, 0, 0 }, 2 },
    };
    const uint8_t* p = (const uint8_t*)buffer;
    size_t lim = bytes < 2048 ? bytes : 2048;
    int bestoff = -1;
    const char* bestname = 0;
    for (size_t k = 0; k < sizeof sy / sizeof sy[0]; k++) {
        for (size_t i = 0; i + (size_t)sy[k].len <= lim; i++) {
            if (memcmp(p + i, sy[k].sig, (size_t)sy[k].len) == 0) {
                if (bestoff < 0 || (int)i < bestoff) { bestoff = (int)i; bestname = sy[k].name; }
                break;
            }
        }
    }
    if (bestoff >= 0) snprintf(out, outsz, "sync=%s@%d", bestname, bestoff);
    else snprintf(out, outsz, "sync=none");
}

/* ---------------- stream table ---------------- */
static haltee_stream* haltee_find_stream(void* stream)
{
    for (int i = 0; i < MAX_STREAMS; i++) {
        if (g_streams[i].used && g_streams[i].stream == stream) return &g_streams[i];
    }
    return 0;
}

static void haltee_dump_words(const char* prefix, uintptr_t a, int words)
{
    char line[900];
    for (int i = 0; i < words; i += 4) {
        size_t o = 0;
        line[0] = 0;
        for (int j = 0; j < 4 && i + j < words; j++) {
            uintptr_t addr = a + (uintptr_t)(i + j) * 4u;
            uint32_t v = haltee_read32(addr);
            const char* nm = haltee_sym_name(v);
            if (!nm) nm = haltee_sym_near(v);
            if (nm) o += (size_t)snprintf(line + o, sizeof line - o, "[%02x]=%08x:%.48s ", (i + j) * 4, v, nm);
            else o += (size_t)snprintf(line + o, sizeof line - o, "[%02x]=%08x ", (i + j) * 4, v);
            if (o > sizeof line - 100) break;
        }
        haltee_log("%s %s", prefix, line);
    }
}

/* logging policy (eMMC wear): one line on a stream's first write, then one
 * summary line per 1000 writes or per 10 s.  Never per write. */
static void haltee_account(haltee_stream* e, const void* buffer, size_t bytes, ssize_t ret)
{
    uint64_t t = haltee_now_ms();
    e->writes++;
    if (bytes > 0) e->bytes += bytes;

    int dosummary = (e->writes == 1) || ((e->writes % 1000) == 0) || (t - e->last_summary_ms >= 10000);
    if (dosummary) {
        e->last_summary_ms = t;
        char head[3 * 33];
        char st[220];
        char sy[80];
        haltee_hex(buffer, bytes, head, sizeof head);
        haltee_pcm_stats(e->format, buffer, bytes, st, sizeof st);
        haltee_find_sync(buffer, bytes, sy, sizeof sy);
        haltee_log("WR idx=%d n=%llu bytes=%zu tot=%llu fmt=0x%08x(%s) rate=%u ch=0x%08x dev=0x%08x fl=0x%x ret=%zd head=%s %s %s",
                   e->idx, (unsigned long long)e->writes, bytes,
                   (unsigned long long)e->bytes, e->format, haltee_fmt_name(e->format),
                   e->rate, e->chmask, e->devices, e->flags, ret, head, st, sy);
    }
    if (e->dump_fd >= 0 && bytes > 0 && e->dump_bytes < e->cap) {
        size_t n = bytes;
        if (e->dump_bytes + n > e->cap) n = (size_t)(e->cap - e->dump_bytes);
        ssize_t w = write(e->dump_fd, buffer, n);
        if (w > 0) e->dump_bytes += (uint64_t)w;
        if (e->dump_bytes >= e->cap) {
            haltee_log("DUMP idx=%d cap reached %llu bytes file=%s",
                       e->idx, (unsigned long long)e->dump_bytes, e->dump_path);
            close(e->dump_fd);
            e->dump_fd = -1;
        }
    }
}

/* ---------------- R3 instrumentation: write granularity + buffer-size override ----------------
 * /data/aes67bridge/timing.enable: collect bounded stats over the next 400 writes of the
 *   player's DIRECT/48k/2ch stream (interval dt histogram + write size distribution),
 *   then emit ONE summary line.  No per-write logging.
 * /data/aes67bridge/latency.frames: if present (e.g. "240" = 5 ms @48k), patch the opened
 *   DIRECT stream's get_buffer_size() to return that many frames, so AudioFlinger
 *   writes smaller chunks (opt-in experiment; remove file + reopen stream to revert).
 * /data/aes67bridge/fifo.pipe_bytes: optional FIFO pipe size override (default 32768).
 */
static unsigned long long haltee_mono_ms(void);
static int haltee_popcount32(uint32_t v);
static int haltee_file_exists(const char* path);

#define TIMING_ENABLE_FILE  "/data/aes67bridge/timing.enable"
#define LATENCY_FRAMES_FILE "/data/aes67bridge/latency.frames"
#define FIFO_PIPE_BYTES     "/data/aes67bridge/fifo.pipe_bytes"
#define TIMING_WINDOW       400

static volatile int g_timing_active = 0;
static volatile void* g_timing_stream = 0;
static unsigned long long g_timing_count = 0;
static unsigned long long g_timing_last_ms = 0;
static unsigned long long g_timing_sum_dt = 0;
static unsigned long long g_timing_min_dt = 0;
static unsigned long long g_timing_max_dt = 0;
static unsigned long long g_timing_dt_hist[8];
static unsigned int g_timing_sizes[8];
static unsigned long long g_timing_size_cnt[8];
static int g_timing_size_n = 0;
static volatile unsigned int g_gbs_frames = 0;
static volatile unsigned int g_gbs_frame_bytes = 0;

static void haltee_timing_reset(void)
{
    g_timing_stream = 0;
    g_timing_count = 0;
    g_timing_last_ms = 0;
    g_timing_sum_dt = 0;
    g_timing_min_dt = ~0ull;
    g_timing_max_dt = 0;
    g_timing_size_n = 0;
    for (int i = 0; i < 8; i++) {
        g_timing_dt_hist[i] = 0;
        g_timing_sizes[i] = 0;
        g_timing_size_cnt[i] = 0;
    }
}

static int haltee_timing_candidate(haltee_stream* e)
{
    if (e->rate != 48000) return 0;
    if (haltee_popcount32(e->chmask) != 2) return 0;
    if (e->format != 1 && e->format != 6) return 0;
    if (!(e->flags & 1u)) return 0;          /* DIRECT only */
    return 1;
}

static void haltee_timing_tick(haltee_stream* e, size_t bytes)
{
    if (!g_timing_active || !haltee_timing_candidate(e)) return;
    if (!g_timing_stream) g_timing_stream = (void*)e;
    else if (g_timing_stream != (void*)e) return;
    unsigned long long now = haltee_mono_ms();
    if (g_timing_last_ms) {
        unsigned long long dt = now - g_timing_last_ms;
        g_timing_sum_dt += dt;
        if (dt < g_timing_min_dt) g_timing_min_dt = dt;
        if (dt > g_timing_max_dt) g_timing_max_dt = dt;
        int b = (dt <= 1) ? 0 : (dt <= 2) ? 1 : (dt <= 5) ? 2 : (dt <= 10) ? 3
              : (dt <= 15) ? 4 : (dt <= 20) ? 5 : (dt <= 25) ? 6 : 7;
        g_timing_dt_hist[b]++;
    }
    g_timing_last_ms = now;
    int found = -1;
    for (int i = 0; i < g_timing_size_n; i++) {
        if (g_timing_sizes[i] == (unsigned int)bytes) { found = i; break; }
    }
    if (found < 0 && g_timing_size_n < 8) {
        found = g_timing_size_n;
        g_timing_sizes[found] = (unsigned int)bytes;
        g_timing_size_cnt[found] = 0;
        g_timing_size_n++;
    }
    if (found >= 0) g_timing_size_cnt[found]++;
    g_timing_count++;
}

static void haltee_timing_dump(const char* why)
{
    if (!g_timing_active) return;
    g_timing_active = 0;
    char sizes[240];
    int off = 0;
    sizes[0] = 0;
    for (int i = 0; i < g_timing_size_n && off < (int)sizeof sizes - 40; i++)
        off += snprintf(sizes + off, sizeof sizes - off, "%uB*%llu ", g_timing_sizes[i], g_timing_size_cnt[i]);
    unsigned long long avg = (g_timing_count > 1) ? g_timing_sum_dt / (g_timing_count - 1) : 0;
    unsigned long long mn = (g_timing_min_dt == ~0ull) ? 0 : g_timing_min_dt;
    haltee_log("TIMING(%s) writes=%llu dt_min=%llums dt_avg=%llums dt_max=%llums hist[<=1,<=2,<=5,<=10,<=15,<=20,<=25,>25]=[%llu,%llu,%llu,%llu,%llu,%llu,%llu,%llu] sizes=%s",
               why, g_timing_count, mn, avg, g_timing_max_dt,
               g_timing_dt_hist[0], g_timing_dt_hist[1], g_timing_dt_hist[2], g_timing_dt_hist[3],
               g_timing_dt_hist[4], g_timing_dt_hist[5], g_timing_dt_hist[6], g_timing_dt_hist[7],
               sizes);
}

static size_t haltee_get_buffer_size(void* stream)
{
    (void)stream;
    return (size_t)g_gbs_frames * (size_t)g_gbs_frame_bytes;
}

static void haltee_gbs_patch(haltee_stream* e, void* stream)
{
    if (!haltee_timing_candidate(e)) return;
    FILE* f = fopen(LATENCY_FRAMES_FILE, "r");
    if (!f) return;
    int frames = 0;
    if (fscanf(f, "%d", &frames) != 1) frames = 0;
    fclose(f);
    if (frames <= 0 || frames > 4800) return;
    unsigned int fb = (e->format == 1) ? 4u : 6u;
    uint32_t cur = haltee_read32((uintptr_t)stream + 8u);   /* audio_stream->get_buffer_size */
    if (!haltee_is_code_ptr(cur)) {
        haltee_log("GBS-SKIP idx=%d get_buffer_size=%p not code ptr", e->idx, (void*)(uintptr_t)cur);
        return;
    }
    g_gbs_frames = (unsigned int)frames;
    g_gbs_frame_bytes = fb;
    int rc = haltee_patch32((uintptr_t)stream + 8u, (uint32_t)(uintptr_t)&haltee_get_buffer_size);
    haltee_log("GBS-PATCH idx=%d frames=%d frame_bytes=%u => %u bytes (orig=%p) rc=%d",
               e->idx, frames, fb, (unsigned)(frames * (int)fb), (void*)(uintptr_t)cur, rc);
}

/* ---------------- v5: compressed bitstream export -> /data/aes67bridge/live.bits ----------------
 * AC3/EAC3/DTS/DTS-HD/TRUEHD/IEC61937 (vendor fmt 0x09..0x0f) are exported as RAW BYTES
 * into FIFO /data/aes67bridge/live.bits, in parallel with the PCM FIFO.  Non-blocking, drop on
 * full, never on disk (except optional debug dump gated by /data/aes67bridge/bits.dump.enable).
 */
static unsigned long long haltee_mono_ms(void);
static int haltee_file_exists(const char* path);
static volatile int g_live_active;

#define LIVE_BITS_FIFO   "/data/aes67bridge/live.bits"
#define LIVE_BITS_META   "/data/aes67bridge/live.bits.meta"
#define BITS_DUMP_ENABLE "/data/aes67bridge/bits.dump.enable"
#define BITS_DUMP_DIR    HALTEE_ROOT "/dump"
#define BITS_DUMP_CAP    (2u * 1024u * 1024u)

static volatile int g_bits_fd = -1;
static volatile void* g_bits_stream = 0;
static volatile unsigned long long g_bits_bytes = 0;
static volatile unsigned long long g_bits_dropped = 0;
static volatile unsigned int g_bits_stream_id = 0xffffffffu;
static volatile unsigned int g_bits_fmt = 0;
static volatile unsigned int g_bits_rate = 0;
static volatile unsigned int g_bits_chmask = 0;
static volatile unsigned long long g_bits_open_ts = 0;
static char g_bits_first[32];
static int g_bits_first_len = 0;
static char g_bits_codec[64] = "unknown";
static unsigned long long g_bits_dump_bytes = 0;
static int g_bits_dump_fd = -1;
static char g_bits_dump_path[160];

static int haltee_bits_codec_fmt(uint32_t fmt)
{
    switch (fmt) {
    case 0x09000000u: case 0x0A000000u: case 0x0B000000u:
    case 0x0C000000u: case 0x0D000000u: case 0x0E000000u:
    case 0x0F000000u:
        return 1;
    default:
        return 0;
    }
}

static void haltee_bits_guess(const uint8_t* p, size_t n, char* out, size_t outsz)
{
    size_t lim = n < 4096 ? n : 4096;
    for (size_t i = 0; i + 4 <= lim; i++) {
        if (p[i] == 0x0B && p[i+1] == 0x77) { snprintf(out, outsz, "ac3/eac3@%zu", i); return; }
        if (p[i] == 0xF8 && p[i+1] == 0x72 && p[i+2] == 0x6F && p[i+3] == 0xBA) { snprintf(out, outsz, "truehd-mat@%zu", i); return; }
        if (p[i] == 0xF8 && p[i+1] == 0x72 && p[i+2] == 0x6F && p[i+3] == 0xBB) { snprintf(out, outsz, "mlp@%zu", i); return; }
        if (p[i] == 0x7F && p[i+1] == 0xFE && p[i+2] == 0x80 && (p[i+3] == 0x01 || p[i+3] == 0x00)) { snprintf(out, outsz, "dts@%zu", i); return; }
        if (p[i] == 0xF8 && p[i+1] == 0x72 && p[i+2] == 0x4E && p[i+3] == 0x1F) { snprintf(out, outsz, "iec61937-preamble@%zu", i); return; }
    }
    snprintf(out, outsz, "unknown(first=%02x%02x%02x%02x)", p[0], n>1?p[1]:0, n>2?p[2]:0, n>3?p[3]:0);
}

static void haltee_bits_write_meta(void)
{
    char hex[3 * 33];
    size_t o = 0;
    hex[0] = 0;
    for (int i = 0; i < g_bits_first_len && o + 4 < sizeof hex; i++)
        o += (size_t)snprintf(hex + o, sizeof hex - o, "%02x", (unsigned char)g_bits_first[i]);
    char buf[1024];
    int n = snprintf(buf, sizeof buf,
        "stream_id=%u\nfmt=0x%08x\nfmt_name=%s\nrate=%u\nchmask=0x%08x\ncodec_guess=%s\n"
        "first_bytes=%s\nts_ms=%llu\nopen_ts_ms=%llu\npid=%d\nbytes=%llu\ndropped=%llu\n",
        g_bits_stream_id, g_bits_fmt, haltee_fmt_name(g_bits_fmt), g_bits_rate, g_bits_chmask,
        g_bits_codec, hex, haltee_mono_ms(), g_bits_open_ts, (int)getpid(),
        (unsigned long long)g_bits_bytes, (unsigned long long)g_bits_dropped);
    if (n <= 0) return;
    char tmp[128];
    snprintf(tmp, sizeof tmp, LIVE_BITS_META ".tmp");
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return;
    ssize_t w = write(fd, buf, (size_t)n);
    (void)w;
    fchmod(fd, 0600);
    close(fd);
    rename(tmp, LIVE_BITS_META);
}

static void haltee_bits_feed(haltee_stream* e, const void* buffer, size_t bytes)
{
    if (!g_live_active || !buffer || bytes == 0) return;
    if (!haltee_bits_codec_fmt(e->format)) return;
    if (!g_bits_stream) {
        g_bits_stream = (void*)e;
        g_bits_stream_id = (unsigned int)e->idx;
        g_bits_fmt = e->format;
        g_bits_rate = e->rate;
        g_bits_chmask = e->chmask;
        g_bits_open_ts = haltee_mono_ms();
        g_bits_first_len = 0;
        snprintf(g_bits_codec, sizeof g_bits_codec, "unknown");
        haltee_log("BITS BIND idx=%d stream=%p fmt=0x%08x(%s) rate=%u chmask=0x%08x flags=0x%x",
                   e->idx, (void*)e, e->format, haltee_fmt_name(e->format), e->rate, e->chmask, e->flags);
    } else if (g_bits_stream != (void*)e) {
        return;
    }
    if (g_bits_first_len == 0) {
        size_t n = bytes < sizeof g_bits_first ? bytes : sizeof g_bits_first;
        memcpy(g_bits_first, buffer, n);
        g_bits_first_len = (int)n;
        haltee_bits_guess((const uint8_t*)buffer, bytes, g_bits_codec, sizeof g_bits_codec);
        char hx[3 * 33];
        size_t o = 0;
        hx[0] = 0;
        for (int i = 0; i < g_bits_first_len && o + 4 < sizeof hx; i++)
            o += (size_t)snprintf(hx + o, sizeof hx - o, "%02x", (unsigned char)g_bits_first[i]);
        haltee_log("BITS FIRST idx=%d fmt=0x%08x(%s) bytes=%zu codec_guess=%s head=%s",
                   e->idx, e->format, haltee_fmt_name(e->format), bytes, g_bits_codec, hx);
        haltee_bits_write_meta();
        if (haltee_file_exists(BITS_DUMP_ENABLE)) {
            mkdir(BITS_DUMP_DIR, 0777);
            snprintf(g_bits_dump_path, sizeof g_bits_dump_path, BITS_DUMP_DIR "/bits_%02d_fmt%08x.bin",
                     e->idx, e->format);
            g_bits_dump_fd = open(g_bits_dump_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
            g_bits_dump_bytes = 0;
            haltee_log("BITS DUMP enabled file=%s cap=%u", g_bits_dump_path, BITS_DUMP_CAP);
        }
    }
    int fd = (int)g_bits_fd;
    if (fd >= 0) {
        ssize_t w = write(fd, buffer, bytes);
        if (w < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                __atomic_add_fetch(&g_bits_dropped, (unsigned long long)bytes, __ATOMIC_RELAXED);
        } else {
            __atomic_add_fetch(&g_bits_bytes, (unsigned long long)w, __ATOMIC_RELAXED);
            if ((size_t)w < bytes)
                __atomic_add_fetch(&g_bits_dropped, (unsigned long long)(bytes - (size_t)w), __ATOMIC_RELAXED);
        }
    }
    if (g_bits_dump_fd >= 0 && g_bits_dump_bytes < BITS_DUMP_CAP) {
        size_t n = bytes;
        if (g_bits_dump_bytes + n > BITS_DUMP_CAP) n = (size_t)(BITS_DUMP_CAP - g_bits_dump_bytes);
        ssize_t w = write(g_bits_dump_fd, buffer, n);
        if (w > 0) g_bits_dump_bytes += (unsigned long long)w;
        if (g_bits_dump_bytes >= BITS_DUMP_CAP) {
            haltee_log("BITS DUMP cap reached file=%s bytes=%llu", g_bits_dump_path,
                       (unsigned long long)g_bits_dump_bytes);
            close(g_bits_dump_fd);
            g_bits_dump_fd = -1;
        }
    }
}

/* ---------------- live PCM export for the AES67 sender (PoC-2) ----------------
 * The HAL process runs as uid audioserver; Android netd blocks AF_INET socket
 * creation for this uid (socket() -> EPERM), so RTP is NOT sent from here.
 * This proxy instead exports 48 kHz / 2 ch PCM, unchanged (AUDIO_FORMAT_PCM_16_BIT
 * or PCM_24_BIT_PACKED, bytes as-is) into the FIFO /data/aes67bridge/live.pcm.
 * The vendor player's DIRECT streams are preferred (bit-exact); when no DIRECT
 * stream is writing, the PRIMARY system mixer is exported instead so audio from
 * third-party apps (ExoPlayer / Kodi / browsers) reaches AES67 too.  A root-side AES67 daemon reads the FIFO,
 * converts to L24 and sends RTP (PT 97, 1 ms packets) to 239.69.2.1:5004.
 *
 * Enabled by /data/aes67bridge/live.enable.  The audio writer thread opens nothing
 * and never blocks: writes to the FIFO are O_NONBLOCK and samples are dropped
 * if the reader is slow or absent.  stream->write() blocking behaviour and its
 * return value are never changed.
 */
#define LIVE_ENABLE_FILE "/data/aes67bridge/live.enable"
#define LIVE_FIFO        "/data/aes67bridge/live.pcm"
#define LIVE_META        "/data/aes67bridge/live.meta"
#define LIVE_SDP         "/data/aes67bridge/haltee.sdp"
#define LIVE_GROUP       "239.69.2.1"
#define LIVE_PORT        5004
#define LIVE_PT          97
#define LIVE_LOCAL_IP    "192.168.63.158"
#define LIVE_FLAG_DIRECT 0x00000001u
#define LIVE_FLAG_PRIMARY 0x00000002u
/* Opt-out for the system-mixer capture: while this file exists only DIRECT
 * streams are exported (original behaviour).  Absent (default) = the PRIMARY
 * mixer output is exported whenever no DIRECT stream is writing, so audio from
 * third-party apps (AUDIO_OUTPUT_FLAG_PRIMARY: ExoPlayer/Kodi/browsers) reaches
 * AES67 too.  The mixer is post-mix and post-volume, so it carries system
 * sounds and follows the Android volume setting. */
#define LIVE_MIXER_DISABLE_FILE "/data/aes67bridge/live.mixer.disable"
/* A different stream may carry the real playback while the bound one
 * (preview/UI/mixer notification) stays idle.  Rebind only after the bound
 * stream has been quiet this long so two streams are never interleaved. */
#define LIVE_SWITCH_SILENCE_MS 200

static volatile int g_live_active = 0;
static volatile int g_live_thread_started = 0;
static volatile int g_live_fd = -1;
static volatile void* g_live_stream = 0;
static volatile unsigned long long g_live_last_write_ms = 0;
static volatile unsigned int g_live_stream_id = 0xffffffffu;
static volatile unsigned int g_live_fmt = 0;
static volatile unsigned long long g_live_bytes = 0;
static volatile unsigned long long g_live_dropped = 0;
static volatile unsigned int g_live_stream_flags = 0;
static volatile int g_live_mixer_ok = 1;

static unsigned long long haltee_mono_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (unsigned long long)ts.tv_sec * 1000ull + (unsigned long long)(ts.tv_nsec / 1000000);
}

static int haltee_path_exists(const char* path)
{
    struct stat st;
    return stat(path, &st) == 0;
}

static int haltee_file_exists(const char* path)
{
    struct stat st;
    return (stat(path, &st) == 0) && S_ISREG(st.st_mode);
}

static int haltee_popcount32(uint32_t v)
{
    int n = 0;
    while (v) { n += (int)(v & 1u); v >>= 1; }
    return n;
}

/* SDP for the root-side sender / PC receiver (PT97 L24/48000/2 multicast) */
static void haltee_live_write_sdp(void)
{
    char sdp[900];
    int n = snprintf(sdp, sizeof sdp,
        "v=0\r\n"
        "o=- %u 0 IN IP4 " LIVE_LOCAL_IP "\r\n"
        "s=HALTEE Z9X AES67 Sender\r\n"
        "i=DIRECT (bit-exact) or system-mixer PCM exported by vendor audio HAL tee (FIFO)\r\n"
        "c=IN IP4 " LIVE_GROUP "/32\r\n"
        "t=0 0\r\n"
        "m=audio %d RTP/AVP %d\r\n"
        "a=rtpmap:%d L24/48000/2\r\n"
        "a=ptime:1\r\n"
        "a=sendonly\r\n"
        "a=ts-refclk:local\r\n"
        "a=mediaclk:direct=0\r\n",
        (unsigned)((unsigned long)time(0) & 0x7ffffffful),
        LIVE_PORT, LIVE_PT, LIVE_PT);
    if (n <= 0) return;
    int fd = open(LIVE_SDP, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) { haltee_log("LIVE sdp open failed errno=%d", errno); return; }
    ssize_t wr = write(fd, sdp, (size_t)n);
    (void)wr;
    fchmod(fd, 0644);
    close(fd);
    haltee_log("LIVE sdp written: %s (%d bytes) PT=%d L24/48000/2", LIVE_SDP, n, LIVE_PT);
}

/* /data/aes67bridge/live.meta - what the FIFO currently carries (read by root daemon) */
static void haltee_live_write_meta_ex(unsigned int sid, unsigned int fmt,
                                      unsigned int rate, unsigned int ch)
{
    char buf[768];
    int n = snprintf(buf, sizeof buf,
        "stream_id=%u\n"
        "fmt=0x%08x\n"
        "fmt_name=%s\n"
        "rate=%u\n"
        "ch=%u\n"
        "ts_ms=%llu\n"
        "open_ts_ms=%llu\n"
        "pid=%d\n"
        "bytes=%llu\n"
        "dropped=%llu\n",
        sid, fmt, haltee_fmt_name(fmt), rate, ch,
        haltee_mono_ms(), haltee_mono_ms(), (int)getpid(),
        (unsigned long long)g_live_bytes, (unsigned long long)g_live_dropped);
    if (n <= 0) return;
    char tmp[128];
    snprintf(tmp, sizeof tmp, LIVE_META ".tmp");
    int fd = open(tmp, O_WRONLY | O_CREAT | O_TRUNC, 0600);
    if (fd < 0) return;
    ssize_t w = write(fd, buf, (size_t)n);
    (void)w;
    fchmod(fd, 0600);
    close(fd);
    rename(tmp, LIVE_META);
}

/* PCM sample size in bytes for the formats exported as-is (0 = unsupported) */
static int haltee_pcm_sample_bytes(uint32_t fmt)
{
    switch (fmt) {
    case 1: return 2;                       /* PCM_16_BIT            */
    case 6: return 3;                       /* PCM_24_BIT_PACKED     */
    case 3:                                 /* PCM_32_BIT            */
    case 4:                                 /* PCM_8_24_BIT          */
    case 5: return 4;                       /* PCM_FLOAT             */
    default: return 0;
    }
}

static int haltee_live_candidate(haltee_stream* e, int* ch_out)
{
    if (e->rate != 44100 && e->rate != 48000) return 0;
    if (!haltee_pcm_sample_bytes(e->format)) return 0;
    int ch = haltee_popcount32(e->chmask);
    if (ch < 2 || ch > 8) return 0;
    if (ch_out) *ch_out = ch;
    return 1;
}

/* DIRECT (official player, bit-exact) wins; the PRIMARY system mixer is the
 * fallback for third-party apps.  Never export both at once: the mixer must
 * not steal an active DIRECT stream, and DIRECT takes over immediately. */
static int haltee_live_allowed(haltee_stream* e)
{
    if (e->flags & LIVE_FLAG_DIRECT) return 1;
    if (!g_live_mixer_ok) return 0;
    return (e->flags & LIVE_FLAG_PRIMARY) ? 1 : 0;
}

/* Create/open the live PCM FIFO the way the live thread does: O_RDWR so the
 * producer never blocks in open(), O_NONBLOCK writes, and the configured pipe
 * size. */
static int haltee_live_open_fifo(void)
{
    if (!haltee_path_exists(LIVE_FIFO)) {
        if (mkfifo(LIVE_FIFO, 0620) != 0 && errno != EEXIST)
            haltee_log("LIVE mkfifo failed errno=%d (%s)", errno, strerror(errno));
    }
    chmod(LIVE_FIFO, 0620);
    int fd = open(LIVE_FIFO, O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (fd < 0) {
        haltee_log("LIVE open fifo failed errno=%d (%s)", errno, strerror(errno));
        return -1;
    }
#ifdef F_SETPIPE_SZ
    int want = 32768;
    FILE* pf = fopen(FIFO_PIPE_BYTES, "r");
    if (pf) {
        int v = 0;
        if (fscanf(pf, "%d", &v) == 1 && v >= 4096 && v <= 32768) want = v;
        fclose(pf);
    }
    if (fcntl(fd, F_SETPIPE_SZ, want) < 0)
        haltee_log("LIVE F_SETPIPE_SZ(%d) failed errno=%d", want, errno);
#endif
    return fd;
}

/* Discard PCM still queued for the previous stream (stale audio the daemon
 * would otherwise decode with the new stream's frame size / rate). */
static void haltee_live_drain_fifo(int fd)
{
    char buf[4096];
    for (int i = 0; i < 64; i++) {
        ssize_t n = read(fd, buf, sizeof buf);
        if (n <= 0) break;
    }
}

static void haltee_live_bind(haltee_stream* e, int ch)
{
    /* Start the new stream on a clean FIFO boundary: drop the old stream's
     * queued bytes, close the writer (the daemon sees EOF, discards a partial
     * frame) and reopen before the first new byte.  This keeps the byte stream
     * frame-aligned across format/rate changes. */
    int old = (int)g_live_fd;
    if (old >= 0) {
        haltee_live_drain_fifo(old);
        close(old);
        g_live_fd = -1;
    }
    g_live_fd = haltee_live_open_fifo();
    g_live_stream = (void*)e;
    g_live_stream_flags = e->flags;
    g_live_stream_id = (unsigned int)e->idx;
    g_live_fmt = e->format;
    haltee_log("LIVE BIND idx=%d stream=%p fmt=0x%08x(%s) rate=%u ch=%d frame_bytes=%d flags=0x%x fifo_fd=%d",
               e->idx, (void*)e, e->format, haltee_fmt_name(e->format), e->rate, ch,
               ch * haltee_pcm_sample_bytes(e->format), e->flags, (int)g_live_fd);
    /* Publish the stream identity BEFORE its first PCM byte reaches the FIFO.
     * Writing meta on stream open made the daemon switch frame size while bytes
     * of the previous stream were still queued; the rest of the stream then
     * decoded byte-shifted (audible noise). */
    haltee_live_write_meta_ex((unsigned int)e->idx, e->format, e->rate, (unsigned int)ch);
}

/* called from the audio writer thread: O_NONBLOCK write, drop on full, never block */
static void haltee_live_feed(haltee_stream* e, const void* buffer, size_t bytes)
{
    if (!g_live_active || !buffer || bytes == 0) return;
    int ch = 0;
    if (!haltee_live_candidate(e, &ch)) return;
    if (!haltee_live_allowed(e)) return;
    unsigned long long now = haltee_mono_ms();
    if (!g_live_stream) {
        haltee_live_bind(e, ch);
    } else if (g_live_stream != (void*)e) {
        int cur_direct = (g_live_stream_flags & LIVE_FLAG_DIRECT) ? 1 : 0;
        int new_direct = (e->flags & LIVE_FLAG_DIRECT) ? 1 : 0;
        unsigned long long quiet = now - g_live_last_write_ms;
        /* Never let the mixer steal an active DIRECT stream; when DIRECT comes
         * back it takes over immediately (bit-exact source wins).  Switching
         * between two streams of the same class waits for silence. */
        if (cur_direct && !new_direct && quiet <= LIVE_SWITCH_SILENCE_MS) return;
        if (!(!cur_direct && new_direct) && quiet <= LIVE_SWITCH_SILENCE_MS) return;
        haltee_log("LIVE rebind idx=%d fmt=0x%08x flags=0x%x (previous idx=%u fmt=0x%08x quiet for %llu ms)",
                   e->idx, e->format, e->flags, g_live_stream_id, g_live_fmt, quiet);
        haltee_live_bind(e, ch);
    }
    g_live_last_write_ms = now;
    int fd = (int)g_live_fd;
    if (fd < 0) return;
    int sample_bytes = haltee_pcm_sample_bytes(e->format);
    if (sample_bytes <= 0) return;
    size_t frame_bytes = (size_t)ch * (size_t)sample_bytes;
    size_t total = bytes - (bytes % frame_bytes);
    if (total < bytes)
        __atomic_add_fetch(&g_live_dropped, (unsigned long long)(bytes - total), __ATOMIC_RELAXED);
    /* Whole frames only, in <=PIPE_BUF chunks: an O_NONBLOCK pipe write of at
     * most PIPE_BUF bytes is atomic (all bytes or EAGAIN), so FIFO pressure can
     * only drop whole frames.  A partial write used to shift every following
     * frame; the daemon then decoded byte-shifted garbage until the stream
     * identity changed (heard as loud noise/pops with multichannel targets,
     * where the extra render+send work makes the reader fall behind). */
    const size_t pipe_atomic = 4096;
    size_t chunk = (pipe_atomic / frame_bytes) * frame_bytes;
    if (chunk == 0) chunk = frame_bytes;
    size_t off = 0;
    while (off < total) {
        size_t n = total - off < chunk ? total - off : chunk;
        ssize_t w = write(fd, (const char*)buffer + off, n);
        if (w < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK)
                __atomic_add_fetch(&g_live_dropped, (unsigned long long)(total - off), __ATOMIC_RELAXED);
            return;
        }
        if (w == 0) return;
        if ((size_t)w != n) {
            size_t whole = ((size_t)w / frame_bytes) * frame_bytes;
            __atomic_add_fetch(&g_live_bytes, (unsigned long long)whole, __ATOMIC_RELAXED);
            __atomic_add_fetch(&g_live_dropped, (unsigned long long)(total - off - whole), __ATOMIC_RELAXED);
            haltee_log("LIVE PARTIAL idx=%d wrote=%zd chunk=%zu frame_bytes=%zu (FIFO alignment risk)",
                       e->idx, w, n, frame_bytes);
            return;
        }
        __atomic_add_fetch(&g_live_bytes, (unsigned long long)n, __ATOMIC_RELAXED);
        off += n;
    }
}

static void* haltee_live_thread(void* arg)
{
    (void)arg;
    unsigned long long last_stat = 0;
    haltee_log("LIVE thread started (FIFO export; RTP is done by root-side daemon)");
    for (;;) {
        usleep(200000);
        unsigned long long now = haltee_mono_ms();
        {
            int twant = haltee_file_exists(TIMING_ENABLE_FILE);
            if (twant && !g_timing_active) {
                haltee_timing_reset();
                g_timing_active = 1;
                haltee_log("TIMING start window=%d writes (DIRECT/48k/2ch)", TIMING_WINDOW);
            } else if (!twant && g_timing_active) {
                haltee_timing_dump("file-removed");
            }
            if (g_timing_active && g_timing_count >= TIMING_WINDOW)
                haltee_timing_dump("window-full");
        }
        {
            int mixer_ok = !haltee_file_exists(LIVE_MIXER_DISABLE_FILE);
            if (mixer_ok != g_live_mixer_ok) {
                g_live_mixer_ok = mixer_ok;
                haltee_log("LIVE mixer capture %s (%s)", mixer_ok ? "enabled" : "disabled",
                           LIVE_MIXER_DISABLE_FILE);
            }
            if (!g_live_mixer_ok && g_live_stream &&
                !(g_live_stream_flags & LIVE_FLAG_DIRECT)) {
                haltee_log("LIVE unbind mixer stream=%p (mixer capture disabled)", (void*)g_live_stream);
                g_live_stream = 0;
            }
        }
        int want = haltee_file_exists(LIVE_ENABLE_FILE);
        if (want && !g_live_active) {
            int fd = haltee_live_open_fifo();
            if (fd < 0) {
                static unsigned long long last_fail = 0;
                if (now - last_fail >= 2000) {
                    last_fail = now;
                    haltee_log("LIVE open fifo failed errno=%d (%s)", errno, strerror(errno));
                }
                continue;
            }
            g_live_fd = fd;
            g_live_bytes = 0;
            g_live_dropped = 0;
            g_live_stream = 0;
            g_live_stream_flags = 0;
            g_live_stream_id = 0xffffffffu;
            g_live_active = 1;
            haltee_live_write_sdp();
            /* v5: open the compressed-bitstream FIFO (independent non-blocking writer) */
            if (!haltee_path_exists(LIVE_BITS_FIFO)) {
                if (mkfifo(LIVE_BITS_FIFO, 0620) != 0 && errno != EEXIST)
                    haltee_log("BITS mkfifo failed errno=%d", errno);
            }
            chmod(LIVE_BITS_FIFO, 0620);
            {
                int bfd = open(LIVE_BITS_FIFO, O_RDWR | O_NONBLOCK | O_CLOEXEC);
                if (bfd >= 0) {
#ifdef F_SETPIPE_SZ
                    int bwant = 32768;
                    if (fcntl(bfd, F_SETPIPE_SZ, bwant) < 0)
                        haltee_log("BITS F_SETPIPE_SZ(%d) failed errno=%d", bwant, errno);
#endif
                    g_bits_fd = bfd;
                    g_bits_stream = 0;
                    g_bits_stream_id = 0xffffffffu;
                    g_bits_bytes = 0;
                    g_bits_dropped = 0;
                    haltee_log("BITS FIFO ENABLED fifo=%s", LIVE_BITS_FIFO);
                } else {
                    haltee_log("BITS open fifo failed errno=%d", errno);
                }
            }
#ifdef F_GETPIPE_SZ
            haltee_log("LIVE ENABLED fifo=%s sdp=%s pipe_sz=%d bytes (PCM as-is; rate 44100/48000, ch 2..8; DIRECT preferred, system mixer fallback unless %s exists; root daemon -> RTP PT%d %s:%d)",
                       LIVE_FIFO, LIVE_SDP, (int)fcntl(fd, F_GETPIPE_SZ), LIVE_MIXER_DISABLE_FILE,
                       LIVE_PT, LIVE_GROUP, LIVE_PORT);
#else
            haltee_log("LIVE ENABLED fifo=%s sdp=%s (PCM as-is; rate 44100/48000, ch 2..8; DIRECT preferred, system mixer fallback unless %s exists; root daemon -> RTP PT%d %s:%d)",
                       LIVE_FIFO, LIVE_SDP, LIVE_MIXER_DISABLE_FILE, LIVE_PT, LIVE_GROUP, LIVE_PORT);
#endif
        } else if (!want && g_live_active) {
            g_live_active = 0;
            g_live_stream = 0;
            g_live_stream_flags = 0;
            int fd = (int)g_live_fd;
            g_live_fd = -1;
            if (fd >= 0) close(fd);
            int bfd = (int)g_bits_fd;
            g_bits_fd = -1;
            if (bfd >= 0) close(bfd);
            if (g_bits_dump_fd >= 0) { close(g_bits_dump_fd); g_bits_dump_fd = -1; }
            haltee_log("LIVE DISABLED pcm_bytes=%llu pcm_dropped=%llu bits_bytes=%llu bits_dropped=%llu",
                       (unsigned long long)g_live_bytes, (unsigned long long)g_live_dropped,
                       (unsigned long long)g_bits_bytes, (unsigned long long)g_bits_dropped);
        } else if (g_live_active) {
            if (g_live_stream && (now - g_live_last_write_ms > 2000)) {
                haltee_log("LIVE unbind idle stream=%p", (void*)g_live_stream);
                g_live_stream = 0;
            }
            if (now - last_stat >= 10000) {
                last_stat = now;
                haltee_log("LIVE STAT pcm_bytes=%llu pcm_dropped=%llu pcm_stream=%u pcm_fmt=0x%08x | bits_bytes=%llu bits_dropped=%llu bits_stream=%u bits_fmt=0x%08x codec=%s",
                           (unsigned long long)g_live_bytes, (unsigned long long)g_live_dropped,
                           g_live_stream_id, g_live_fmt,
                           (unsigned long long)g_bits_bytes, (unsigned long long)g_bits_dropped,
                           g_bits_stream_id, g_bits_fmt, g_bits_codec);
            }
        }
    }
    return 0;
}

static void haltee_live_start(void)
{
    if (g_live_thread_started) return;
    g_live_thread_started = 1;
    pthread_t t;
    if (pthread_create(&t, 0, haltee_live_thread, 0) == 0) {
        pthread_detach(t);
        haltee_log("LIVE thread created (daemon)");
    } else {
        haltee_log("LIVE thread create FAILED errno=%d", errno);
    }
}

static ssize_t haltee_write(void* stream, const void* buffer, size_t bytes)
{
    haltee_stream* e = haltee_find_stream(stream);
    if (!e || !e->orig_write) {
        if (g_unknown_writes < 5) {
            g_unknown_writes++;
            haltee_log("WR-UNKNOWN stream=%p bytes=%zu (tee table miss)", stream, bytes);
        }
        return -EINVAL;
    }
    ssize_t ret = e->orig_write(stream, buffer, bytes);
    haltee_account(e, buffer, bytes, ret);
    haltee_timing_tick(e, bytes);
    haltee_live_feed(e, buffer, bytes > 0 ? bytes : 0);
    haltee_bits_feed(e, buffer, bytes > 0 ? bytes : 0);
    return ret;
}

/* ---------------- device / stream wrapping ---------------- */
static void haltee_wrap_stream(void* stream, struct haltee_config* cfg, uint32_t handle,
                               uint32_t devices, uint32_t flags, const char* address)
{
    haltee_stream* e = haltee_find_stream(stream);
    if (!e) {
        for (int i = 0; i < MAX_STREAMS; i++) {
            if (!g_streams[i].used) { e = &g_streams[i]; break; }
        }
    }
    if (!e) {
        haltee_log("WRAP-FAIL table full stream=%p", stream);
        return;
    }
    memset(e, 0, sizeof *e);
    e->used = 1;
    e->idx = g_stream_seq++;
    e->stream = stream;
    e->dump_fd = -1;
    e->handle = handle;
    e->devices = devices;
    e->flags = flags;
    e->format = cfg ? cfg->format : 0;
    e->rate = cfg ? cfg->sample_rate : 0;
    e->chmask = cfg ? cfg->channel_mask : 0;
    if (address) strncpy(e->address, address, sizeof e->address - 1);

    char fl[120];
    haltee_flags_str(flags, fl, sizeof fl);
    haltee_log("OPEN idx=%d stream=%p handle=%u fmt=0x%08x(%s) rate=%u chmask=0x%08x dev=0x%08x fl=0x%x(%s) addr='%s'",
               e->idx, stream, handle, e->format, haltee_fmt_name(e->format),
               e->rate, e->chmask, devices, flags, fl, e->address);

    haltee_refresh_maps();
    if (e->idx < 2) haltee_dump_words("STREAM", (uintptr_t)stream, 48);

    /* locate write pointer: ABI offset first, then a name-based scan */
    int off = -1;
    const char* how = "abi";
    uint32_t v = haltee_read32((uintptr_t)stream + STREAM_OUT_OFF_WRITE);
    if (haltee_is_code_ptr(v)) {
        off = (int)STREAM_OUT_OFF_WRITE;
    } else {
        for (int o = 0; o < 384; o += 4) {
            uint32_t w = haltee_read32((uintptr_t)stream + (uintptr_t)o);
            const char* nm = haltee_sym_name(w);
            if (haltee_is_code_ptr(w) && nm &&
                (strstr(nm, "out_write") || strstr(nm, "bitstream"))) {
                off = o;
                how = "scan";
                v = w;
                break;
            }
        }
    }
    if (off < 0) {
        haltee_log("NOWRAP idx=%d write pointer not found (tee off for this stream)", e->idx);
        return;
    }
    e->write_off = off;
    e->orig_write = (stream_write_fn)(uintptr_t)v;
    int rc = haltee_patch32((uintptr_t)stream + (uintptr_t)off, (uint32_t)(uintptr_t)&haltee_write);
    const char* wn = haltee_sym_name(v);
    if (!wn) wn = haltee_sym_near(v);
    haltee_log("WRAP idx=%d write_off=%d(%s) orig=%p(%s) patch_rc=%d",
               e->idx, off, how, (void*)(uintptr_t)v, wn ? wn : "?", rc);
    if (rc != 0) {
        e->orig_write = 0;
        return;
    }

    /* raw dump is OFF by default (eMMC wear): only when /data/aes67bridge/dump.enable exists */
    if (haltee_file_exists(DUMP_ENABLE_FILE)) {
        mkdir(HALTEE_ROOT, 0775);
        mkdir(HALTEE_DUMP, 0700);
        e->cap = (uint64_t)DUMP_CAP;
        snprintf(e->dump_path, sizeof e->dump_path, HALTEE_DUMP "/s%02d_fmt%08x.bin", e->idx, e->format);
        e->dump_fd = open(e->dump_path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
        haltee_log("DUMP idx=%d file=%s cap=%llu fd=%d (dump.enable present)", e->idx, e->dump_path,
                   (unsigned long long)e->cap, e->dump_fd);
    } else {
        e->cap = 0;
        e->dump_fd = -1;
    }
    haltee_gbs_patch(e, stream);
    if (g_live_active && haltee_bits_codec_fmt(e->format)) {
        g_bits_stream_id = (unsigned int)e->idx;
        g_bits_fmt = e->format;
        g_bits_rate = e->rate;
        g_bits_chmask = e->chmask;
        g_bits_open_ts = haltee_mono_ms();
        g_bits_first_len = 0;
        snprintf(g_bits_codec, sizeof g_bits_codec, "pending");
        haltee_bits_write_meta();
        haltee_log("BITS meta updated on open idx=%d fmt=0x%08x(%s) rate=%u chmask=0x%08x",
                   e->idx, e->format, haltee_fmt_name(e->format), e->rate, e->chmask);
    }
    {
        int meta_ch = 0;
        /* Only refresh meta for the stream that currently owns the FIFO.  A
         * freshly opened stream publishes its meta from haltee_live_bind() on
         * its first write, i.e. when it really starts feeding the FIFO. */
        if (g_live_active && (void*)e == g_live_stream && haltee_live_candidate(e, &meta_ch)) {
            haltee_live_write_meta_ex((unsigned int)e->idx, e->format, e->rate, (unsigned int)meta_ch);
            haltee_log("LIVE meta refreshed on bound stream open idx=%d fmt=0x%08x(%s) rate=%u ch=%d frame_bytes=%d",
                       e->idx, e->format, haltee_fmt_name(e->format), e->rate, meta_ch,
                       meta_ch * haltee_pcm_sample_bytes(e->format));
        }
    }
}

static int haltee_open_out(void* dev, uint32_t handle, uint32_t devices, uint32_t flags,
                           struct haltee_config* cfg, void** stream_out, const char* address)
{
    if (!g_orig_open_out) {
        haltee_log("OPENOUT called but orig pointer missing");
        return -ENOSYS;
    }
    int rc = g_orig_open_out(dev, handle, devices, flags, cfg, stream_out, address);
    haltee_log("OPENOUT rc=%d handle=%u dev=0x%08x fl=0x%x stream_out=%p",
               rc, handle, devices, flags, stream_out ? *stream_out : 0);
    if (rc == 0 && stream_out && *stream_out) {
        haltee_wrap_stream(*stream_out, cfg, handle, devices, flags, address);
    }
    return rc;
}

static void haltee_patch_device(void* dev)
{
    haltee_refresh_maps();
    haltee_log("DEV refresh maps: n=%d dev=%p", g_maps_n, dev);
    haltee_dump_words("DEV", (uintptr_t)dev, 64);
    uint32_t sym_open = (uint32_t)(uintptr_t)dlsym(g_orig_handle, "adev_open_output_stream_new");
    haltee_log("DEV dlsym(adev_open_output_stream_new)=%p", (void*)(uintptr_t)sym_open);

    int off = -1;
    if (sym_open) {
        if (haltee_read32((uintptr_t)dev + DEV_OFF_OPEN_OUTPUT_STREAM) == sym_open) {
            off = (int)DEV_OFF_OPEN_OUTPUT_STREAM;
        } else {
            for (int o = 4; o < 512; o += 4) {
                uint32_t w = haltee_read32((uintptr_t)dev + (uintptr_t)o);
                if (w && w == sym_open) { off = o; break; }
            }
        }
    }
    if (off < 0) {
        /* last resort: scan by symbol name pattern */
        for (int o = 4; o < 512; o += 4) {
            uint32_t w = haltee_read32((uintptr_t)dev + (uintptr_t)o);
            const char* nm = haltee_sym_name(w);
            if (haltee_is_code_ptr(w) && nm && strstr(nm, "open_output_stream")) {
                off = o;
                haltee_log("DEV name-scan hit off=%d sym=%s", o, nm);
                break;
            }
        }
    }
    if (off < 0) {
        uint32_t w = haltee_read32((uintptr_t)dev + DEV_OFF_OPEN_OUTPUT_STREAM);
        const char* nm = haltee_sym_name(w);
        if (haltee_is_code_ptr(w) && (!nm || strstr(nm, "open_output_stream"))) {
            off = (int)DEV_OFF_OPEN_OUTPUT_STREAM;
            haltee_log("DEV using ABI offset fallback (ptr=%p sym=%s)", (void*)(uintptr_t)w, nm ? nm : "?");
        }
    }
    if (off < 0) {
        haltee_log("DEV-PATCH-FAIL open_output_stream not located; tee disabled");
        return;
    }
    uint32_t cur = haltee_read32((uintptr_t)dev + (uintptr_t)off);
    g_orig_open_out = (open_out_fn)(uintptr_t)cur;
    g_open_out_off = off;
    int rc = haltee_patch32((uintptr_t)dev + (uintptr_t)off, (uint32_t)(uintptr_t)&haltee_open_out);
    const char* nm = haltee_sym_name(cur);
    if (!nm) nm = haltee_sym_near(cur);
    haltee_log("DEV-PATCH open_output_stream off=%d orig=%p(%s) patch_rc=%d", off,
               (void*)(uintptr_t)cur, nm ? nm : "?", rc);
    if (rc == 0) g_open_out_patched = 1;
}

/* ---------------- module entry ---------------- */
static int haltee_find_base_cb(struct dl_phdr_info* info, size_t sz, void* data)
{
    (void)sz;
    (void)data;
    if (!info->dlpi_name || !strstr(info->dlpi_name, "audio.primary.amlogic.so.orig")) return 0;
    g_base = (uintptr_t)info->dlpi_addr;
    uintptr_t end = g_base;
    for (int i = 0; i < info->dlpi_phnum; i++) {
        if (info->dlpi_phdr[i].p_type != PT_LOAD) continue;
        uintptr_t e = g_base + (uintptr_t)info->dlpi_phdr[i].p_vaddr +
                      (uintptr_t)info->dlpi_phdr[i].p_memsz;
        if (e > end) end = e;
    }
    g_base_end = end;
    return 1;
}

static int haltee_dev_open(const hw_module_t* module, const char* id, hw_device_t** device)
{
    (void)module;
    if (!g_init_done) {
        g_init_done = 1;
        g_maps_n = -1;
        mkdir(HALTEE_ROOT, 0775);
        mkdir(HALTEE_LOGDIR, 0700);
        mkdir(HALTEE_DUMP, 0700);
        void* lh = dlopen("liblog.so", RTLD_NOW | RTLD_LOCAL);
        if (lh) g_log_print = (int (*)(int, const char*, const char*, ...))dlsym(lh, "__android_log_print");
        haltee_log("=== haltee proxy v5-live-r2 (dir=/data/aes67bridge) loaded: pid=%d uid=%d euid=%d ===",
                   (int)getpid(), (int)getuid(), (int)geteuid());
    }
    haltee_log("PROXY dev_open id=%s", id ? id : "(null)");

    g_orig_handle = dlopen(ORIG_PATH, RTLD_NOW | RTLD_LOCAL);
    if (!g_orig_handle) {
        haltee_log("FATAL dlopen(%s) failed: %s", ORIG_PATH, dlerror());
        return -ENOENT;
    }
    haltee_log("dlopen orig ok handle=%p", g_orig_handle);

    hw_module_t* real = (hw_module_t*)dlsym(g_orig_handle, "HMI");
    if (!real) {
        haltee_log("FATAL dlsym(HMI) failed: %s", dlerror());
        return -ENOENT;
    }
    haltee_log("orig HMI=%p tag=0x%08x api=%u/%u id='%s' name='%s' methods=%p",
               (void*)real, real->tag, real->module_api_version, real->hal_api_version,
               real->id ? real->id : "?", real->name ? real->name : "?", (void*)real->methods);

    if (real->tag != HARDWARE_MODULE_TAG) {
        haltee_log("FATAL orig HMI tag mismatch");
        return -ENOENT;
    }
    if (!real->methods || !real->methods->open) {
        haltee_log("FATAL orig HMI has no methods->open");
        return -ENOENT;
    }

    dl_iterate_phdr(haltee_find_base_cb, 0);
    haltee_load_maps();
    haltee_live_start();
    haltee_log("orig base=%p end=%p", (void*)g_base, (void*)g_base_end);

    int rc = real->methods->open(real, id, device);
    haltee_log("orig module->open rc=%d dev=%p", rc, (device && *device) ? (void*)*device : 0);
    if (rc == 0 && device && *device) {
        if (haltee_read32((uintptr_t)*device) != HARDWARE_DEVICE_TAG) {
            haltee_log("WARN device tag=0x%08x (expected 0x%08x)",
                       haltee_read32((uintptr_t)*device), HARDWARE_DEVICE_TAG);
        }
        haltee_patch_device(*device);
    }
    return rc;
}

/* module info: hw_get_module() does dlsym(handle, "HMI") */
static hw_module_methods_t haltee_methods = { .open = haltee_dev_open };

struct audio_module HMI = {
    .common = {
        .tag = HARDWARE_MODULE_TAG,
        .module_api_version = 0x0001,
        .hal_api_version = 0x0100,
        .id = "audio",
        .name = "Amlogic audio primary HAL (haltee proxy)",
        .author = "haltee PoC-1",
        .methods = &haltee_methods,
        .dso = 0,
        .reserved = { 0 },
    },
};
