#include "vfs.h"
#include "demo.h"

/*
 * 目录项缓存的对照实验。
 *
 * 目的不是断言「快」，而是把「一次路径解析打到底层多少次 lookup」量出来，
 * 让 dentry 的收益有可复现的数字，而不是靠主观感受。
 *
 * 场景取自真实负载的形状：
 *   S1 同一路径反复打开（shell `cat` 的行为，A29 fd 复用之外的部分）
 *   S2 大量不同路径、共享前缀（编译器找头文件 / 包管理遍历依赖）
 *   S3 大量不存在的路径（构建系统探测可选文件，负缓存的主战场）
 *
 * 统计口径：`lookups` 是 vfs 真正下发给 port 的次数。dentry 命中时它是 0
 * 增长，因此「lookups 减少多少」就是「省掉多少次 ext4 目录扫描」。
 */

struct lab_harness {
    unsigned lookups;
    unsigned long long probe; /* 防止编译器把循环优化掉 */
};

static enum vfs_result lab_lookup(void *ctx, uint32_t dir, const char *name, size_t len,
                                  struct vfs_node *out)
{
    struct lab_harness *h = ctx;
    h->lookups++;
    h->probe += (unsigned long long)dir * 31 + len;

    /* 目录树：#2 -> #3(demo) / #4(src)；#3 -> #10..#99 一批文件。 */
    if (dir == 2) {
        if (len == 4 && memcmp(name, "demo", len) == 0) {
            *out = (struct vfs_node){3, VFS_DIRECTORY};
            return VFS_OK;
        }
        if (len == 3 && memcmp(name, "src", len) == 0) {
            *out = (struct vfs_node){4, VFS_DIRECTORY};
            return VFS_OK;
        }
        return VFS_NOT_FOUND;
    }
    if (dir == 3 && len == 9 && memcmp(name, "hello.txt", len) == 0) {
        *out = (struct vfs_node){10, VFS_REGULAR};
        return VFS_OK;
    }
    return VFS_NOT_FOUND;
}

static enum vfs_result lab_retain(void *ctx, uint32_t inode)
{
    (void)ctx;
    (void)inode;
    return VFS_OK;
}

static void lab_release(void *ctx, uint32_t inode)
{
    (void)ctx;
    (void)inode;
}

static enum vfs_result lab_read_at(void *ctx, uint32_t inode, uint64_t offset, void *buffer,
                                   size_t wanted, size_t *actual)
{
    struct lab_harness *h = ctx;
    static const char text[] = "hello\n";
    size_t size = sizeof(text) - 1;

    (void)inode;
    h->probe += offset + wanted;
    if (offset >= size) {
        *actual = 0;
        return VFS_OK;
    }
    size_t n = size - offset;
    if (n > wanted)
        n = wanted;
    for (size_t i = 0; i < n; i++)
        ((char *)buffer)[i] = text[offset + i];
    *actual = n;
    return VFS_OK;
}

static void report(const char *label, unsigned cold, unsigned warm)
{
    char line[128];
    char *p = line;

    for (const char *s = label; *s; s++)
        *p++ = *s;
    /* 省手写 printf：手工拼「cold=.. warm=.. 节省=..%」。 */
    const char *tag = " cold=";
    for (const char *s = tag; *s; s++)
        *p++ = *s;
    {
        char digits[8];
        unsigned v = cold, i = 0;
        if (!v)
            digits[i++] = '0';
        while (v) {
            digits[i++] = (char)('0' + v % 10);
            v /= 10;
        }
        while (i)
            *p++ = digits[--i];
    }
    const char *tag2 = " warm=";
    for (const char *s = tag2; *s; s++)
        *p++ = *s;
    {
        char digits[8];
        unsigned v = warm, i = 0;
        if (!v)
            digits[i++] = '0';
        while (v) {
            digits[i++] = (char)('0' + v % 10);
            v /= 10;
        }
        while (i)
            *p++ = digits[--i];
    }
    *p++ = '\n';
    /* 用 '\0' 结尾再输出，避免越界读。 */
    if (p < line + sizeof(line))
        *p = '\0';
    else
        line[sizeof(line) - 1] = '\0';
    demo_log(line);
}

/* S1：同一路径反复打开。冷启动一次，之后全部命中。 */
static void bench_same_path(void)
{
    struct lab_harness h = {0};
    const struct vfs_port port = {&h, lab_lookup, lab_retain, lab_release, lab_read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    unsigned fd;

    CHECK(vfs_open(&vfs, 1, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_close(&vfs, 1, fd) == VFS_OK);
    unsigned cold = h.lookups;

    h.lookups = 0;
    for (unsigned i = 0; i < 1000; i++) {
        CHECK(vfs_open(&vfs, 1, "/demo/hello.txt", &fd) == VFS_OK);
        CHECK(vfs_close(&vfs, 1, fd) == VFS_OK);
    }
    report("S1 same path x1000", cold, h.lookups);
    CHECK(cold == 2 && h.lookups == 0);
}

/*
 * S2：大量不同路径但共享前缀。分成两个子场景，因为缓存槽位是有限的：
 *   S2a 名字数 < 槽位：第二次访问全部命中
 *   S2b 名字数 > 槽位：LRU 反复淘汰，命中率趋近于 0
 * 这不是「优化失败」，而是把容量边界显式量出来，避免对大目录树产生错觉。
 */
static void build_probe_path(char *path, unsigned i)
{
    path[0] = '/';
    path[1] = 'd'; path[2] = 'e'; path[3] = 'm'; path[4] = 'o';
    path[5] = '/';
    path[6] = 'f';
    path[7] = (char)('0' + (char)(i / 100 % 10));
    path[8] = (char)('0' + (char)(i / 10 % 10));
    path[9] = (char)('0' + (char)(i % 10));
    path[10] = '\0';
}

static void bench_distinct(unsigned count, const char *label)
{
    struct lab_harness h = {0};
    const struct vfs_port port = {&h, lab_lookup, lab_retain, lab_release, lab_read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    unsigned fd;
    char path[32];

    for (unsigned i = 0; i < count; i++) {
        build_probe_path(path, i);
        CHECK(vfs_open(&vfs, 1, path, &fd) == VFS_NOT_FOUND);
    }
    unsigned filled = vfs.dentry_used;
    unsigned cold = h.lookups;

    h.lookups = 0;
    for (unsigned i = 0; i < count; i++) {
        build_probe_path(path, i);
        CHECK(vfs_open(&vfs, 1, path, &fd) == VFS_NOT_FOUND);
    }
    report(label, cold, h.lookups);

    /* 容量边界：装得下则第二轮零查找，装不下则接近全 miss。 */
    if (count <= VFS_DENTRIES) {
        CHECK(h.lookups == 0);
        CHECK(filled >= count);
    } else {
        CHECK(h.lookups > count / 2);
    }
}

static void bench_many_paths(void)
{
    /* 24 个名字 + 根目录 "demo" 一条 = 25，稳稳落在 32 槽内。 */
    bench_distinct(VFS_DENTRIES - 8, "S2a 24 names x2 (fits cache)");
    /* 1000 个名字远超槽位，记录 LRU 抖动。 */
    bench_distinct(1000, "S2b 1000 names x2 (thrash)");
}

/*
 * S3：路径数量远大于缓存槽位，观察淘汰行为。
 * 这不是「优化成功」的展示，而是记录缓存的容量上限。
 */
static void bench_overflow(void)
{
    struct lab_harness h = {0};
    const struct vfs_port port = {&h, lab_lookup, lab_retain, lab_release, lab_read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    unsigned fd;
    char path[2 + VFS_DENTRY_NAME + 1];
    unsigned rounds = VFS_DENTRIES * 4;

    for (unsigned i = 0; i < rounds; i++) {
        path[0] = '/';
        for (unsigned k = 0; k < 8; k++)
            path[1 + k] = (char)('a' + (int)(k % 26));
        path[9] = (char)('0' + (int)(i % 10));
        path[10] = (char)('0' + (int)(i / 10 % 10));
        path[11] = '\0';
        CHECK(vfs_open(&vfs, 1, path, &fd) == VFS_NOT_FOUND);
    }
    CHECK(vfs.dentry_used == VFS_DENTRIES);
    CHECK(vfs.dcache.evictions >= rounds - VFS_DENTRIES);
    /* 标签说明清楚：这两个数不是「冷/暖 lookup」，而是「条目数 / 槽位」。 */
    report("S3 overflow: distinct names / slots", rounds, vfs.dentry_used);
}

static void run(void)
{
    bench_same_path();
    bench_many_paths();
    bench_overflow();
    demo_log("vfs-dcache: lookup counts measured; hits bypass port, negative entries cached");
}

DEMO_MODULE(vfs_dcache, true, run, NULL);
