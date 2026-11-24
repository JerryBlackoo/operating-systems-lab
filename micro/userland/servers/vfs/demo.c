#include "vfs.h"
#include "demo.h"

struct directory_fixture {
    unsigned refs, reads;
    bool fail_read, fail_retain, overread;
};

static enum vfs_result lookup(void *ctx, uint32_t dir, const char *name, size_t len,
                              struct vfs_node *out)
{
    (void)ctx;
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
    struct directory_fixture *fs = ctx;
    CHECK(inode == 4);
    if (fs->fail_retain)
        return VFS_IO_ERROR;
    fs->refs++;
    return VFS_OK;
}

static void release(void *ctx, uint32_t inode)
{
    struct directory_fixture *fs = ctx;
    CHECK(inode == 4 && fs->refs);
    fs->refs--;
}

static enum vfs_result read_at(void *ctx, uint32_t inode, uint64_t offset, void *buffer,
                               size_t wanted, size_t *actual)
{
    struct directory_fixture *fs = ctx;
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

static void check_paths(struct vfs *vfs, struct directory_fixture *fs)
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

static void check_reads(struct vfs *vfs, struct directory_fixture *fs)
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

static void check_clients(struct vfs *vfs, struct directory_fixture *fs)
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

static void check_exhaustion(struct vfs *vfs, struct directory_fixture *fs)
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

static void run(void)
{
    struct directory_fixture fs = {0};
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
    demo_log("vfs: per-client fd tables, independent offsets, short read, EOF and exhaustion");
}

DEMO_MODULE(vfs, true, run, NULL);
