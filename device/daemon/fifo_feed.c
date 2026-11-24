/* fifo_feed.c —— 合成测试源：把 raw PCM 以固定块（默认 5ms/20ms）写进测试 FIFO，并写 live.meta */
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <stdint.h>
#include <sys/stat.h>

static volatile int g_stop = 0;
static void on_sig(int s) { (void)s; g_stop = 1; }
static long long mono_ms(void) { struct timespec ts; clock_gettime(CLOCK_MONOTONIC, &ts); return (long long)ts.tv_sec*1000LL + ts.tv_nsec/1000000LL; }
static void sleep_until_ms(long long due) {
    struct timespec ts; ts.tv_sec = (time_t)(due/1000); ts.tv_nsec = (long)((due%1000)*1000000LL);
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL) == EINTR && !g_stop) {}
}
static void write_meta(const char* path, int sid, int fmt, int rate, int ch, long long bytes, long long dropped) {
    char tmp[512], buf[1024];
    snprintf(tmp, sizeof tmp, "%s.tmp", path);
    long long now = mono_ms();
    int n = snprintf(buf, sizeof buf,
        "stream_id=%d\nfmt=0x%08x\nfmt_name=%s\nrate=%d\nch=%d\nts_ms=%lld\nopen_ts_ms=%lld\npid=%d\nbytes=%lld\ndropped=%lld\n",
        sid, fmt, (fmt == 6) ? "PCM_24_BIT_PACKED" : "PCM_16_BIT", rate, ch, now, now, (int)getpid(), bytes, dropped);
    FILE* f = fopen(tmp, "w"); if (!f) return;
    fwrite(buf, 1, (size_t)n, f); fclose(f);
    rename(tmp, path);
}

int main(int argc, char** argv) {
    const char* file = NULL; const char* fifo = NULL; const char* meta = NULL;
    int rate = 48000, ch = 2, fmt = 1, chunk_ms = 5, sid = 9001;   /* fmt: 1=S16, 6=S24 packed */
    double hold = 1.0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--file") && i+1<argc) file = argv[++i];
        else if (!strcmp(argv[i], "--fifo") && i+1<argc) fifo = argv[++i];
        else if (!strcmp(argv[i], "--meta") && i+1<argc) meta = argv[++i];
        else if (!strcmp(argv[i], "--rate") && i+1<argc) rate = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--ch") && i+1<argc) ch = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--fmt") && i+1<argc) {
            const char* v = argv[++i];
            if (!strcmp(v, "s16") || !strcmp(v, "1")) fmt = 1;
            else if (!strcmp(v, "s24") || !strcmp(v, "6")) fmt = 6;
            else fmt = atoi(v);
        }
        else if (!strcmp(argv[i], "--chunk-ms") && i+1<argc) chunk_ms = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--stream-id") && i+1<argc) sid = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--hold") && i+1<argc) hold = atof(argv[++i]);
    }
    if (!file || !fifo || !meta) { fprintf(stderr, "usage: %s --file raw --fifo F --meta M [--rate R --ch C --chunk-ms MS --stream-id N --hold S]\n", argv[0]); return 2; }
    int bps = (fmt == 1) ? 2 : 3;   /* S16(1) 或 S24 packed(6) */
    FILE* f = fopen(file, "rb"); if (!f) { perror("open raw"); return 2; }
    fseek(f, 0, SEEK_END); long sz = ftell(f); fseek(f, 0, SEEK_SET);
    uint8_t* data = malloc((size_t)sz); if (fread(data, 1, (size_t)sz, f) != (size_t)sz) { perror("read"); return 2; }
    fclose(f);
    size_t frame_bytes = (size_t)bps * ch;
    long long total_frames = sz / (long)frame_bytes;
    int fifo_fd = -1;
    for (int i = 0; i < 50 && !g_stop; i++) {   /* 等读者打开（最多 5s） */
        fifo_fd = open(fifo, O_WRONLY | O_NONBLOCK);
        if (fifo_fd >= 0) break;
        usleep(100000);
    }
    if (fifo_fd < 0) { fprintf(stderr, "open fifo failed errno=%d\n", errno); return 2; }
    signal(SIGINT, on_sig); signal(SIGTERM, on_sig);
    int chunk_frames = rate * chunk_ms / 1000; if (chunk_frames < 1) chunk_frames = 1;
    size_t chunk_bytes = (size_t)chunk_frames * frame_bytes;
    long long bytes_written = 0, dropped = 0, frames_done = 0;
    write_meta(meta, sid, fmt, rate, ch, 0, 0);
    fprintf(stderr, "FEED start sd=%d rate=%d ch=%d fmt=%d frames=%lld chunk=%dms\n", sid, rate, ch, fmt, total_frames, chunk_ms);
    long long t0 = mono_ms();
    long long chunk_index = 0;
    while (frames_done < total_frames && !g_stop) {
        sleep_until_ms(t0 + chunk_index * chunk_ms);
        size_t off = (size_t)frames_done * frame_bytes;
        size_t want = chunk_bytes; size_t rem = (size_t)(sz - (long)off); if (want > rem) want = rem;
        size_t done = 0;
        while (done < want && !g_stop) {
            ssize_t w = write(fifo_fd, data + off + done, want - done);
            if (w > 0) done += (size_t)w;
            else if (w < 0 && errno == EAGAIN) { dropped += (long long)(want - done); break; }
            else break;
        }
        bytes_written += (long long)done;
        frames_done += (long long)(done / frame_bytes);
        chunk_index++;
        if ((chunk_index % 200) == 0) write_meta(meta, sid, fmt, rate, ch, bytes_written, dropped);
    }
    write_meta(meta, sid, fmt, rate, ch, bytes_written, dropped);
    fprintf(stderr, "FEED done frames=%lld/%lld bytes=%lld dropped_bytes=%lld\n", frames_done, total_frames, bytes_written, dropped);
    usleep((useconds_t)(hold * 1e6));
    close(fifo_fd);
    free(data);
    return 0;
}
