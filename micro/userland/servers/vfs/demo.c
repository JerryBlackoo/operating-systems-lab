#include "vfs.h"
#include "demo.h"

struct directory_fixture { unsigned refs; bool fail_read; };

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
    CHECK(inode == 4 && fs->refs);
    if (fs->fail_read)
        return VFS_IO_ERROR;
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

static void run(void)
{
    struct directory_fixture fs = {0};
    const struct vfs_port port = {&fs, lookup, retain, release, read_at};
    struct vfs vfs = {.port = &port, .root = 2};
    unsigned a, b;
    char data[8];
    size_t n;

    CHECK(vfs_open(&vfs, 10, "/demo/../hello.txt", &a) == VFS_BAD_PATH);
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &a) == VFS_OK);
    CHECK(vfs_open(&vfs, 10, "/demo/hello.txt", &b) == VFS_OK && a != b && fs.refs == 2);
    CHECK(vfs_read(&vfs, 11, a, data, 2, &n) == VFS_BAD_FD);
    CHECK(vfs_read(&vfs, 10, a, data, 2, &n) == VFS_OK && n == 2 && !memcmp(data, "he", n));
    CHECK(vfs_read(&vfs, 10, b, data, 3, &n) == VFS_OK && n == 3 && !memcmp(data, "hel", n));
    fs.fail_read = true;
    CHECK(vfs_read(&vfs, 10, a, data, 8, &n) == VFS_IO_ERROR);
    fs.fail_read = false;
    CHECK(vfs_read(&vfs, 10, a, data, 8, &n) == VFS_OK && n == 3 && !memcmp(data, "llo", n));
    CHECK(vfs_read(&vfs, 10, a, data, 8, &n) == VFS_OK && n == 0);
    CHECK(vfs_close(&vfs, 10, a) == VFS_OK);
    CHECK(vfs_read(&vfs, 10, a, data, 8, &n) == VFS_BAD_FD);
    CHECK(vfs_close(&vfs, 10, b) == VFS_OK && fs.refs == 0);
    demo_log("vfs: component lookup, independent offsets, short read and EOF");
}

DEMO_MODULE(vfs, true, run, NULL);
