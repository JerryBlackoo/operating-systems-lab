#ifndef MICRO_MODEL_EXT4_H
#define MICRO_MODEL_EXT4_H

#include <stddef.h>
#include <stdint.h>

enum { EXT4_BLOCK = 4096, EXT4_ROOT_INODE = 2, EXT4_GROUPS = 8, EXT4_NAME_MAX = 255 };
enum ext4_result {
    EXT4_OK, EXT4_BAD_ARG, EXT4_FORMAT, EXT4_UNSUPPORTED,
    EXT4_NOT_MOUNTED, EXT4_NOT_FOUND, EXT4_WRONG_TYPE, EXT4_BUSY,
};
enum ext4_type { EXT4_FILE, EXT4_DIRECTORY };

struct ext4_node { uint32_t ino; enum ext4_type type; };
struct ext4 {
    const uint8_t *image;
    uint32_t blocks, inodes, inodes_per_group;
    uint16_t inode_size;
    uint32_t inode_tables[EXT4_GROUPS];
};

enum ext4_result ext4_mount(struct ext4 *, const void *image, size_t bytes);
void ext4_unmount(struct ext4 *);
enum ext4_result ext4_lookup(const struct ext4 *, uint32_t dir, const char *name,
                            size_t name_len, struct ext4_node *);
enum ext4_result ext4_read_at(const struct ext4 *, uint32_t ino, uint64_t offset,
                             void *dst, size_t len, size_t *read);

#endif
