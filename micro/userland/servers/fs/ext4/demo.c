#include "ext4.h"
#include "demo.h"

enum {
    IMAGE_BLOCKS = 32, FILE_INO = 18, SECOND_INO = 3,
    FILE_BYTES = 3 * EXT4_BLOCK + 1,
};
static uint8_t image[IMAGE_BLOCKS * EXT4_BLOCK];

static void put16(uint8_t *p, uint16_t value)
{
    p[0] = value;
    p[1] = value >> 8;
}

static void put32(uint8_t *p, uint32_t value)
{
    put16(p, value);
    put16(p + 2, value >> 16);
}

static uint8_t *at(unsigned block)
{
    return image + EXT4_BLOCK * block;
}

static uint8_t *make_inode(unsigned table, unsigned slot, uint16_t mode, uint32_t size,
                           uint16_t extent_count)
{
    uint8_t *inode = at(table) + 128 * slot;
    put16(inode, mode);
    put32(inode + 4, size);
    put32(inode + 0x20, 0x80000);
    uint8_t *ext = inode + 0x28;
    put16(ext, 0xf30a);
    put16(ext + 2, extent_count);
    put16(ext + 4, 4);
    return ext + 12;
}

static void make_extent(uint8_t *entry, uint32_t logical, uint32_t physical, uint16_t len)
{
    put32(entry, logical);
    put16(entry + 4, len);
    put32(entry + 8, physical);
}

static void make_dirent(uint8_t *entry, uint32_t ino, uint16_t size, uint8_t type,
                        const char *name, uint8_t len)
{
    put32(entry, ino);
    put16(entry + 4, size);
    entry[6] = len;
    entry[7] = type;
    memcpy(entry + 8, name, len);
}

static void make_image(void)
{
    memset(image, 0, sizeof(image));
    uint8_t *super = image + 1024;
    put32(super, 32);
    put32(super + 4, IMAGE_BLOCKS);
    put32(super + 0x18, 2);
    put32(super + 0x1c, 2);
    put32(super + 0x20, 16);
    put32(super + 0x28, 16);
    put16(super + 0x38, 0xef53);
    put16(super + 0x3a, 1);
    put16(super + 0x58, 128);
    put32(super + 0x60, 0x42);
    put32(at(1) + 8, 4);
    put32(at(1) + 32 + 8, 19);

    uint8_t *root = make_inode(4, 1, 0x41ed, 2 * EXT4_BLOCK, 2);
    make_extent(root, 0, 6, 1);
    make_extent(root + 12, 1, 9, 1);
    make_dirent(at(6), 2, 12, 2, ".", 1);
    make_dirent(at(6) + 12, 2, 12, 2, "..", 2);
    make_dirent(at(6) + 24, 0, 12, 0, "", 0);
    make_dirent(at(6) + 36, FILE_INO, EXT4_BLOCK - 36, 1, "hello.txt", 9);
    make_dirent(at(9), SECOND_INO, EXT4_BLOCK, 1, "second.txt", 10);
    make_inode(4, 2, 0x81a4, 0, 0);

    uint8_t *file = make_inode(19, 1, 0x81a4, FILE_BYTES, 3);
    make_extent(file, 0, 8, 1);
    make_extent(file + 12, 1, 25, 1);
    make_extent(file + 24, 3, 26, 0x8001);
    memcpy(at(8) + EXT4_BLOCK - 2, "AB", 2);
    memcpy(at(25), "CD", 2);
    memset(at(26), 0xee, EXT4_BLOCK);
}

static enum ext4_result mount_fresh(void)
{
    struct ext4 probe = {0};
    enum ext4_result result = ext4_mount(&probe, image, sizeof(image));
    if (result == EXT4_OK)
        ext4_unmount(&probe);
    return result;
}

static void run(void)
{
    struct ext4 fs = {0};
    struct ext4_node file;
    char data[8];
    size_t n;

    make_image();
    CHECK(ext4_mount(&fs, NULL, sizeof(image)) == EXT4_BAD_ARG);
    CHECK(ext4_mount(&fs, image, 4095) == EXT4_BAD_ARG);
    CHECK(ext4_mount(&fs, image, sizeof(image) - 1) == EXT4_FORMAT && !fs.image);
    CHECK(ext4_mount(&fs, image, sizeof(image)) == EXT4_OK);
    CHECK(ext4_mount(&fs, image, sizeof(image)) == EXT4_BUSY);

    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "hello.txt", 9, &file) == EXT4_OK);
    CHECK(file.ino == FILE_INO && file.type == EXT4_FILE);
    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "second.txt", 10, &file) == EXT4_OK);
    CHECK(file.ino == SECOND_INO && file.type == EXT4_FILE);
    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "hellox", 6, &file) == EXT4_NOT_FOUND);
    CHECK(ext4_lookup(&fs, FILE_INO, "hello.txt", 9, &file) == EXT4_WRONG_TYPE);
    CHECK(ext4_read_at(&fs, EXT4_ROOT_INODE, 0, data, 1, &n) == EXT4_WRONG_TYPE);

    CHECK(ext4_read_at(&fs, FILE_INO, EXT4_BLOCK - 2, data, 4, &n) == EXT4_OK);
    CHECK(n == 4 && !memcmp(data, "ABCD", n));
    CHECK(ext4_read_at(&fs, FILE_INO, EXT4_BLOCK * 2, data, 4, &n) == EXT4_OK);
    CHECK(n == 4 && !memcmp(data, "\0\0\0\0", n));
    CHECK(ext4_read_at(&fs, FILE_INO, FILE_BYTES - 1, data, 4, &n) == EXT4_OK);
    CHECK(n == 1 && data[0] == 0);
    CHECK(ext4_read_at(&fs, FILE_INO, FILE_BYTES, data, 4, &n) == EXT4_OK && n == 0);
    CHECK(ext4_read_at(&fs, SECOND_INO, 0, data, 4, &n) == EXT4_OK && n == 0);

    put16(at(6) + 36 + 4, 0);
    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "hello.txt", 9, &file) == EXT4_FORMAT);
    put16(at(6) + 36 + 4, 14);
    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "hello.txt", 9, &file) == EXT4_FORMAT);
    put16(at(6) + 36 + 4, EXT4_BLOCK - 36);
    put32(at(19) + 128 + 0x28 + 12 + 8, IMAGE_BLOCKS);
    CHECK(ext4_read_at(&fs, FILE_INO, 0, data, 1, &n) == EXT4_FORMAT);
    ext4_unmount(&fs);
    CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "hello.txt", 9, &file) == EXT4_NOT_MOUNTED);
    CHECK(ext4_read_at(&fs, FILE_INO, 0, data, 1, &n) == EXT4_NOT_MOUNTED);

    make_image();
    put16(image + 1024 + 0x38, 0);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put16(image + 1024 + 0x3a, 2);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put32(image + 1024 + 0x18, 1);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put32(image + 1024 + 0x1c, 1);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put16(image + 1024 + 0xfe, 64);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put32(image + 1024 + 0x60, 0x2);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put32(image + 1024 + 0x64, 0x200);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put32(image + 1024 + 0x60, 0x242);
    CHECK(mount_fresh() == EXT4_OK);

    make_image();
    put32(image + 1024 + 0x20, 0);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put32(image + 1024 + 4, IMAGE_BLOCKS + 1);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put16(image + 1024 + 0x58, 100);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put32(at(1) + 8, IMAGE_BLOCKS);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put32(at(4) + 128 + 4, 1000);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put16(at(4) + 128, 0x81a4);
    CHECK(mount_fresh() == EXT4_FORMAT);

    make_image();
    put16(at(4) + 128 + 0x28 + 6, 1);
    CHECK(mount_fresh() == EXT4_UNSUPPORTED);

    make_image();
    put32(at(19) + 128 + 0x20, 0);
    CHECK(ext4_mount(&fs, image, sizeof(image)) == EXT4_OK);
    {
        struct ext4_node ignored;
        CHECK(ext4_read_at(&fs, FILE_INO, 0, data, 1, &n) == EXT4_UNSUPPORTED);
        CHECK(ext4_lookup(&fs, EXT4_ROOT_INODE, "second.txt", 10, &ignored) == EXT4_OK);
    }
    ext4_unmount(&fs);

    make_image();
    put32(at(19) + 128 + 0x20, 0x10080000);
    CHECK(ext4_mount(&fs, image, sizeof(image)) == EXT4_OK);
    CHECK(ext4_read_at(&fs, FILE_INO, 0, data, 1, &n) == EXT4_UNSUPPORTED);
    ext4_unmount(&fs);

    make_image();
    put32(at(19) + 128 + 0x28 + 12 + 12, 0);
    CHECK(ext4_mount(&fs, image, sizeof(image)) == EXT4_OK);
    CHECK(ext4_read_at(&fs, FILE_INO, 0, data, 1, &n) == EXT4_FORMAT);
    ext4_unmount(&fs);

    demo_log("ext4: superblock geometry, inode groups, directory records, split extents, sparse and unwritten reads");
}

DEMO_MODULE(ext4, true, run, NULL);
