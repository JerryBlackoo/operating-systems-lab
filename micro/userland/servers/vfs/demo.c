#include "vfs.h"
#include "demo.h"

struct directory_harness {
    unsigned refs, reads, lookups;
    bool fail_read, fail_retain, overread;
};

static enum vfs_result lookup(void *ctx, uint32_t dir, const char *name, size_t len,
                              struct vfs_node *out)
{
    struct directory_harness *fs = ctx;
    fs->lookups++;
    if (dir == 2 && len == 4 && memcmp(name, "demo", len) == 0)
        *out = (struct vfs_node){3, VFS_DIRECTORY};
    else if (dir == 3 && len == 9 && memcmp(name, "hello.txt", len) == 0)
        *out = (struct vfs_node){4, VFS_REGULAR};
    else
        return VFS_NOT_FOUND;
    return VFS_OK;
}

static enum vfs_result retain(void *ctx, uint32_t inode)
{
    struct directory_harness *fs = ctx;
    CHECK(inode == 4);
    if (fs->fail_retain)
        return VFS_IO_ERROR;
    fs->refs++;
    return VFS_OK;
}

static void release(void *ctx, uint32_t inode)
{
    struct directory_harness *fs = ctx;
    CHECK(inode == 4 && fs->refs);
    fs->refs--;
}

static enum vfs_result read_at(void *ctx, uint32_t inode, uint64_t offset, void *buffer,
                               size_t wanted, size_t *actual)
{
    struct directory_harness *fs = ctx;
    static const char contents[] = "hello";
    const size_t size = sizeof(contents) - 1;
    CHECK(inode == 4 && fs->refs && wanted);
    fs->reads++;
    if (fs->fail_read)
        return VFS_IO_ERROR;
    if (fs->overread) {
        *actual = wanted + 1;
        return VFS_OK;
    }
    if (offset >= size) {
        *actual = 0;
        return VFS_OK;
    }
    size_t count = size - offset;
    if (count > wanted)
        count = wanted;
    memcpy(buffer, contents + offset, count);
    *actual = count;
    return VFS_OK;
}

static void check_paths(struct vfs *vfs, struct directory_harness *fs)
{
    unsigned fd;
    char long_path[2 + VFS_NAME_MAX + 1] = "/";

    memset(long_path + 1, 'a', VFS_NAME_MAX + 1);
    long_path[sizeof(long_path) - 1] = 0;

    CHECK(vfs_open(vfs, 10, NULL, &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "demo/hello.txt", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "//demo/hello.txt", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "/demo//hello.txt", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "/demo/", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "/demo/../hello.txt", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "/./demo/hello.txt", &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, long_path, &fd) == VFS_BAD_PATH);
    CHECK(vfs_open(vfs, 10, "/", &fd) == VFS_NOT_FILE);
    CHECK(vfs_open(vfs, 10, "/demo", &fd) == VFS_NOT_FILE);
    CHECK(vfs_open(vfs, 10, "/demo/hello.txt/", &fd) == VFS_NOT_DIR);
    CHECK(vfs_open(vfs, 10, "/demo/hello.txt/x", &fd) == VFS_NOT_DIR);
    CHECK(vfs_open(vfs, 10, "/missing", &fd) == VFS_NOT_FOUND);
    CHECK(fs->refs == 0);
}

static void check_reads(struct vfs *vfs, struct directory_harness *fs)
{
    unsigned a, b;
    char data[8];
    size_t n = 99;

    CHECK(vfs_open(vfs, 10, "/demo/hello.txt", &a) == VFS_OK && a == 0);
    CHECK(vfs_open(vfs, 10, "/demo/hello.txt", &b) == VFS_OK && b == 1 && a != b);
    CHECK(fs->refs == 2);

    CHECK(vfs_read(vfs, 10, VFS_FILES, data, 2, &n) == VFS_BAD_FD && n == 0);
    CHECK(vfs_read(vfs, 11, a, data, 2, &n) == VFS_BAD_FD && n == 0);
    CHECK(vfs_read(vfs, 10, a, data, 2, NULL) == VFS_BAD_ARG);
    CHECK(vfs_read(vfs, 10, a, NULL, 2, &n) == VFS_BAD_ARG && n == 0);

    /* pread：显式 offset 读取，不推进描述符偏移。 */
    CHECK(vfs_read_at(vfs, 10, a, 1, data, 3, &n) == VFS_OK && n == 3 &&
          !memcmp(data, "ell", n));
    CHECK(vfs_read_at(vfs, 10, a, 4, data, 4, &n) == VFS_OK && n == 1 &&
          data[0] == 'o');
    CHECK(vfs_read_at(vfs, 10, a, 99, data, 4, &n) == VFS_OK && n == 0);
    CHECK(vfs_read_at(vfs, 10, a, 0, data, 2, NULL) == VFS_BAD_ARG);
    CHECK(vfs_read_at(vfs, 11, a, 0, data, 2, &n) == VFS_BAD_FD && n == 0);

    unsigned reads = fs->reads;
    CHECK(vfs_read(vfs, 10, a, NULL, 0, &n) == VFS_OK && n == 0 && fs->reads == reads);

    CHECK(vfs_read(vfs, 10, a, data, 2, &n) == VFS_OK && n == 2 && !memcmp(data, "he", n));
    CHECK(vfs_read(vfs, 10, b, data, 3, &n) == VFS_OK && n == 3 && !memcmp(data, "hel", n));

    fs->overread = true;
    CHECK(vfs_read(vfs, 10, a, data, 2, &n) == VFS_IO_ERROR && n == 0);
    fs->overread = false;
    CHECK(vfs_read(vfs, 10, a, data, 2, &n) == VFS_OK && n == 2 && !memcmp(data, "ll", n));
    CHECK(vfs_read(vfs, 10, b, data, 8, &n) == VFS_OK && n == 2 && !memcmp(data, "lo", n));

    fs->fail_read = true;
    CHECK(vfs_read(vfs, 10, a, data, 8, &n) == VFS_IO_ERROR && n == 0);
    fs->fail_read = false;
    CHECK(vfs_read(vfs, 10, a, data, 8, &n) == VFS_OK && n == 1 && data[0] == 'o');
    CHECK(vfs_read(vfs, 10, a, data, 8, &n) == VFS_OK && n == 0);

    CHECK(vfs_close(vfs, 11, a) == VFS_BAD_FD);
    CHECK(vfs_close(vfs, 10, VFS_FILES) == VFS_BAD_FD);
    CHECK(vfs_close(vfs, 10, b) == VFS_OK && fs->refs == 1);
    CHECK(vfs_close(vfs, 10, b) == VFS_BAD_FD);
    CHECK(vfs_read(vfs, 10, b, data, 8, &n) == VFS_BAD_FD && n == 0);
    CHECK(vfs_close(vfs, 10, a) == VFS_OK && fs->refs == 0);
}

static void check_clients(struct vfs *vfs, struct directory_harness *fs)
{
    unsigned c, d;
    char data[8];
    size_t n = 99;

    CHECK(vfs_open(vfs, 11, "/demo/hello.txt", &c) == VFS_OK && c == 0);
    CHECK(vfs_open(vfs, 12, "/demo/hello.txt", &d) == VFS_OK && d == 0 && c == d);
    CHECK(vfs_read(vfs, 11, c, data, 2, &n) == VFS_OK && n == 2 && !memcmp(data, "he", n));
    CHECK(vfs_read(vfs, 12, d, data, 4, &n) == VFS_OK && n == 4 && !memcmp(data, "hell", n));
    CHECK(vfs_close(vfs, 11, c) == VFS_OK && fs->refs == 1);
    CHECK(vfs_read(vfs, 11, d, data, 8, &n) == VFS_BAD_FD && n == 0);
    CHECK(vfs_close(vfs, 12, d) == VFS_OK && fs->refs == 0);
}

static void check_exhaustion(struct vfs *vfs, struct directory_harness *fs)
{
    unsigned fd, spare;

    for (unsigned i = 0; i < VFS_FILES; i++)
        CHECK(vfs_open(vfs, 10, "/demo/hello.txt", &fd) == VFS_OK && fd == i);
    CHECK(fs->refs == VFS_FILES);
    CHECK(vfs_open(vfs, 10, "/demo/hello.txt", &fd) == VFS_NO_SPACE && fs->refs == VFS_FILES);
    CHECK(vfs_close(vfs, 10, 3) == VFS_OK && fs->refs == VFS_FILES - 1);
    CHECK(vfs_open(vfs, 10, "/demo/hello.txt", &fd) == VFS_OK && fd == 3);
    CHECK(fs->refs == VFS_FILES);

    CHECK(vfs_open(vfs, 11, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_open(vfs, 12, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_open(vfs, 13, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_open(vfs, 14, "/demo/hello.txt", &fd) == VFS_NO_SPACE);
    CHECK(fs->refs == VFS_FILES + 3);

    CHECK(vfs_close(vfs, 11, 0) == VFS_OK && fs->refs == VFS_FILES + 2);
    CHECK(vfs_open(vfs, 14, "/demo/hello.txt", &fd) == VFS_OK && fd == 0);

    CHECK(vfs_close(vfs, 12, 0) == VFS_OK && fs->refs == VFS_FILES + 2);
    fs->fail_retain = true;
    CHECK(vfs_open(vfs, 11, "/demo/hello.txt", &fd) == VFS_IO_ERROR);
    CHECK(fs->refs == VFS_FILES + 2);
    fs->fail_retain = false;
    CHECK(vfs_open(vfs, 15, "/demo/hello.txt", &spare) == VFS_OK && spare == 0);
    CHECK(fs->refs == VFS_FILES + 3);

    for (unsigned i = 0; i < VFS_FILES; i++)
        CHECK(vfs_close(vfs, 10, i) == VFS_OK);
    CHECK(vfs_close(vfs, 13, 0) == VFS_OK);
    CHECK(vfs_close(vfs, 14, 0) == VFS_OK);
    CHECK(vfs_close(vfs, 15, 0) == VFS_OK && fs->refs == 0);

    for (unsigned i = 0; i < VFS_CLIENTS; i++)
        CHECK(!vfs->clients[i].id && !vfs->clients[i].open);
}

/*
 * 目录项缓存：验证「命中不再触碰 port」「负缓存」「超长名绕过」
 * 「flush / 按父目录失效」四条语义。缓存只影响开销，不允许影响结果。
 */
static void check_dcache(void)
{
    struct directory_harness fs = {0};
    const struct vfs_port port = {&fs, lookup, retain, release, read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    unsigned fd;
    char long_name[2 + VFS_NAME_MAX + 1] = "/";
    char data[8];
    size_t n;

    /* ① 冷启动：两个分量各自未命中一次。 */
    unsigned before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(fs.lookups == before + 2);
    CHECK(vfs.dcache.misses == 2 && vfs.dcache.hits == 0);
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ② 同一路径再来一次：两级全部命中，port 一次都没被调用。 */
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(fs.lookups == before);
    CHECK(vfs.dcache.hits == 2 && vfs.dcache.misses == 2);
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ③ 命中缓存返回的 inode/type 必须和真实一致（读得到正确内容）。 */
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_read(&vfs, 10, fd, data, 5, &n) == VFS_OK && n == 5 &&
          !memcmp(data, "hello", n));
    CHECK(fs.lookups == before);
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ④ 负缓存：第一次未命中走 port，第二次直接命中 negative 条目。 */
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/nope", &fd) == VFS_NOT_FOUND);
    CHECK(fs.lookups == before + 1);
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/nope", &fd) == VFS_NOT_FOUND);
    CHECK(fs.lookups == before);
    CHECK(vfs.dcache.negative_hits == 1);

    /* ⑤ 负缓存不跨父目录串味：/demo/nope 仍是新的未命中。 */
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/nope", &fd) == VFS_NOT_FOUND);
    CHECK(fs.lookups == before + 1);

    /* ⑥ 超长分量（>VFS_DENTRY_NAME 但 <=VFS_NAME_MAX）不进缓存，每次都走 port。 */
    memset(long_name + 1, 'b', VFS_DENTRY_NAME + 4);
    long_name[1 + VFS_DENTRY_NAME + 4] = 0;
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, long_name, &fd) == VFS_NOT_FOUND);
    CHECK(fs.lookups == before + 1);
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, long_name, &fd) == VFS_NOT_FOUND);
    CHECK(fs.lookups == before + 1);
    CHECK(vfs.dcache.bypass == 2);

    /* ⑦ flush 之后必须重新走 port，但计数器不被清零（它记录历史总量）。 */
    unsigned misses = vfs.dcache.misses, hits = vfs.dcache.hits;
    vfs_dcache_flush(&vfs);
    CHECK(vfs.dentry_used == 0);
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(fs.lookups == before + 2);
    CHECK(vfs.dcache.misses == misses + 2 && vfs.dcache.hits == hits);
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ⑧ 按父目录失效：只丢 parent==3 的条目，根目录那条留下。 */
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK); /* 重新填充 */
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);
    unsigned used = vfs.dentry_used;
    vfs_dcache_invalidate_parent(&vfs, 3);
    CHECK(vfs.dentry_used == used - 1);
    before = fs.lookups;
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK); /* "demo" 命中，仅 "hello.txt" 重查 */
    CHECK(fs.lookups == before + 1);
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ⑨ 槽位压力：塞满后仍在用（LRU 淘汰），且结果依然正确。 */
    for (unsigned round = 0; round < VFS_DENTRIES * 3; round++) {
        char probe[2 + VFS_DENTRY_NAME + 1] = "/";
        memset(probe + 1, 'a' + (int)(round % 26), 8);
        probe[9] = 0;
        probe[8] = (char)('0' + (int)(round % 10));
        CHECK(vfs_open(&vfs, 10, probe, &fd) == VFS_NOT_FOUND);
    }
    CHECK(vfs.dentry_used == VFS_DENTRIES);
    CHECK(vfs.dcache.evictions >= VFS_DENTRIES * 2);
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &fd) == VFS_OK);
    CHECK(vfs_read(&vfs, 10, fd, data, 5, &n) == VFS_OK && n == 5 &&
          !memcmp(data, "hello", n));
    CHECK(vfs_close(&vfs, 10, fd) == VFS_OK);

    /* ⑩ 缓存不改变 refs 收支：全部关闭后引用计数归零。 */
    CHECK(fs.refs == 0);
}

static void run(void)
{
    struct directory_harness fs = {0};
    const struct vfs_port port = {&fs, lookup, retain, release, read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    struct vfs empty = {0};
    unsigned fd;

    CHECK(vfs_open(NULL, 10, "/demo/hello.txt", &fd) == VFS_BAD_ARG);
    CHECK(vfs_open(&empty, 10, "/demo/hello.txt", &fd) == VFS_BAD_ARG);
    CHECK(vfs_open(&vfs, 0, "/demo/hello.txt", &fd) == VFS_BAD_ARG);
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", NULL) == VFS_BAD_ARG);
    CHECK(vfs_read(&empty, 10, 0, NULL, 0, NULL) == VFS_BAD_ARG);
    CHECK(vfs_close(&empty, 10, 0) == VFS_BAD_ARG);

    check_paths(&vfs, &fs);
    check_reads(&vfs, &fs);
    check_clients(&vfs, &fs);
    check_exhaustion(&vfs, &fs);
    check_dcache();
    demo_log("vfs: per-client fd tables, independent offsets, short read, EOF and exhaustion");
    demo_log("vfs: dentry cache hits bypass port; negative entries, bypass and flush verified");
}

DEMO_MODULE(vfs, true, run, NULL);
