#include "vfs.h"

/*
 * 定长名字比较／拷贝。走显式循环而不是 memcmp/memcpy，避免本模块依赖外部
 * 运行时的符号（它在 host demo 与 U-mode 服务两处都要单独链接）。
 */
static bool name_equal(const char *left, const char *right, size_t len)
{
    for (size_t i = 0; i < len; i++) {
        if ((unsigned char)left[i] != (unsigned char)right[i])
            return false;
    }
    return true;
}

static void name_copy(char *dst, const char *src, size_t len)
{
    for (size_t i = 0; i < len; i++)
        dst[i] = src[i];
}

/* ---- 目录项缓存 ---- */

static struct vfs_dentry *dentry_find(struct vfs *vfs, uint32_t parent, const char *name,
                                     size_t len)
{
    if (len > VFS_DENTRY_NAME)
        return NULL;
    for (unsigned i = 0; i < VFS_DENTRIES; i++) {
        struct vfs_dentry *d = &vfs->dentries[i];
        if (d->used && d->parent == parent && (size_t)d->name_len == len &&
            name_equal(d->name, name, len)) {
            return d;
        }
    }
    return NULL;
}

/* 取一个可写槽位：优先空槽，否则最老（stamp 最小）的一条。 */
static struct vfs_dentry *dentry_slot(struct vfs *vfs)
{
    struct vfs_dentry *oldest = &vfs->dentries[0];

    for (unsigned i = 0; i < VFS_DENTRIES; i++) {
        struct vfs_dentry *d = &vfs->dentries[i];
        if (!d->used)
            return d;
        if (d->stamp < oldest->stamp)
            oldest = d;
    }
    vfs->dcache.evictions++;
    return oldest;
}

static void dentry_touch(struct vfs *vfs, struct vfs_dentry *d)
{
    d->stamp = ++vfs->dentry_stamp;
    if (vfs->dentry_stamp == 0) {
        /* 回绕：全表重置，避免所有条目 stamp 相等导致 LRU 退化成随机。 */
        for (unsigned i = 0; i < VFS_DENTRIES; i++)
            vfs->dentries[i].stamp = 0;
        d->stamp = vfs->dentry_stamp = 1;
    }
}

/*
 * 记录一次解析结果。positive 条目缓存 inode 与类型，negative 条目缓存
 * 「这个名字在该目录下不存在」。调用方保证 len <= VFS_DENTRY_NAME。
 */
static void dentry_store(struct vfs *vfs, uint32_t parent, const char *name, size_t len,
                         const struct vfs_node *node, bool negative)
{
    struct vfs_dentry *d = dentry_find(vfs, parent, name, len);

    if (!d) {
        d = dentry_slot(vfs);
        if (!d->used)
            vfs->dentry_used++;
        d->used = true;
        d->parent = parent;
        d->name_len = (uint8_t)len;
        name_copy(d->name, name, len);
    }
    d->negative = negative;
    d->inode = negative ? 0 : node->inode;
    d->type = negative ? VFS_REGULAR : node->type;
    dentry_touch(vfs, d);
}

void vfs_dcache_flush(struct vfs *vfs)
{
    if (!vfs)
        return;
    for (unsigned i = 0; i < VFS_DENTRIES; i++)
        vfs->dentries[i].used = false;
    vfs->dentry_used = 0;
}

void vfs_dcache_invalidate_parent(struct vfs *vfs, uint32_t parent)
{
    if (!vfs)
        return;
    for (unsigned i = 0; i < VFS_DENTRIES; i++) {
        struct vfs_dentry *d = &vfs->dentries[i];
        if (d->used && d->parent == parent) {
            d->used = false;
            vfs->dentry_used--;
        }
    }
}

/*
 * 解析单个分量。命中缓存时不再触碰 port；未命中才调用 lookup，并把
 * 「找到」或「不存在」都记进缓存（I/O 错误不缓存，避免把一次临时故障固化）。
 */
static enum vfs_result resolve_component(struct vfs *vfs, uint32_t parent, const char *name,
                                        size_t len, struct vfs_node *node)
{
    struct vfs_dentry *d = dentry_find(vfs, parent, name, len);

    if (d) {
        if (d->negative) {
            vfs->dcache.negative_hits++;
            return VFS_NOT_FOUND;
        }
        vfs->dcache.hits++;
        node->inode = d->inode;
        node->type = d->type;
        dentry_touch(vfs, d);
        return VFS_OK;
    }

    if (len > VFS_DENTRY_NAME) {
        /* 超长名字不进缓存，但照常解析。 */
        vfs->dcache.bypass++;
        return vfs->port->lookup(vfs->port->ctx, parent, name, len, node);
    }

    vfs->dcache.misses++;
    enum vfs_result result = vfs->port->lookup(vfs->port->ctx, parent, name, len, node);
    if (result == VFS_OK)
        dentry_store(vfs, parent, name, len, node, false);
    else if (result == VFS_NOT_FOUND) {
        struct vfs_node unused = {0, VFS_REGULAR};
        dentry_store(vfs, parent, name, len, &unused, true);
    }
    return result;
}

static enum vfs_result resolve(struct vfs *vfs, const char *name, struct vfs_node *node)
{
    *node = (struct vfs_node){.inode = vfs->root, .type = VFS_DIRECTORY};
    for (;;) {
        const char *end = name;
        while (*end && *end != '/')
            end++;
        size_t len = (size_t)(end - name);
        if (!len || len > VFS_NAME_MAX ||
            (name[0] == '.' && (len == 1 || (len == 2 && name[1] == '.'))))
            return VFS_BAD_PATH;
        enum vfs_result result = resolve_component(vfs, node->inode, name, len, node);
        if (result != VFS_OK)
            return result;
        if (!*end)
            return node->type == VFS_REGULAR ? VFS_OK : VFS_NOT_FILE;
        if (node->type != VFS_DIRECTORY)
            return VFS_NOT_DIR;
        name = end + 1;
    }
}

static struct vfs_client *find_client(struct vfs *vfs, uint32_t client)
{
    for (unsigned i = 0; i < VFS_CLIENTS; i++) {
        if (vfs->clients[i].id == client)
            return &vfs->clients[i];
    }
    return NULL;
}

static struct vfs_client *find_free_client(struct vfs *vfs)
{
    for (unsigned i = 0; i < VFS_CLIENTS; i++) {
        if (!vfs->clients[i].id)
            return &vfs->clients[i];
    }
    return NULL;
}

static bool port_ready(const struct vfs_port *port)
{
    return port && port->lookup && port->retain && port->release && port->read_at;
}

enum vfs_result vfs_open(struct vfs *vfs, uint32_t client, const char *path, unsigned *fd)
{
    if (!vfs || !port_ready(vfs->port) || !client || !fd)
        return VFS_BAD_ARG;
    if (!path || path[0] != '/')
        return VFS_BAD_PATH;
    if (!path[1])
        return VFS_NOT_FILE;

    struct vfs_node node;
    enum vfs_result result = resolve(vfs, path + 1, &node);
    if (result != VFS_OK)
        return result;

    struct vfs_client *owner = find_client(vfs, client);
    if (!owner)
        owner = find_free_client(vfs);
    if (!owner)
        return VFS_NO_SPACE;

    unsigned slot = VFS_FILES;
    for (unsigned i = 0; i < VFS_FILES; i++) {
        if (!owner->files[i].used) {
            slot = i;
            break;
        }
    }
    if (slot == VFS_FILES)
        return VFS_NO_SPACE;

    result = vfs->port->retain(vfs->port->ctx, node.inode);
    if (result != VFS_OK)
        return result;

    owner->id = client;
    owner->files[slot] = (struct vfs_file){.inode = node.inode, .used = true};
    owner->open++;
    *fd = slot;
    return VFS_OK;
}

enum vfs_result vfs_read(struct vfs *vfs, uint32_t client, unsigned fd, void *buffer,
                         size_t count, size_t *actual)
{
    if (!vfs || !vfs->port || !vfs->port->read_at || !actual)
        return VFS_BAD_ARG;
    *actual = 0;
    if (!client)
        return VFS_BAD_FD;

    struct vfs_client *owner = find_client(vfs, client);
    if (!owner || fd >= VFS_FILES || !owner->files[fd].used)
        return VFS_BAD_FD;
    if (count && !buffer)
        return VFS_BAD_ARG;
    if (!count)
        return VFS_OK;

    struct vfs_file *file = &owner->files[fd];
    size_t n = 0;
    enum vfs_result result = vfs->port->read_at(vfs->port->ctx, file->inode, file->offset,
                                                buffer, count, &n);
    if (result != VFS_OK)
        return result;
    if (n > count || n > UINT64_MAX - file->offset)
        return VFS_IO_ERROR;
    file->offset += n;
    *actual = n;
    return VFS_OK;
}

enum vfs_result vfs_read_at(struct vfs *vfs, uint32_t client, unsigned fd, uint64_t offset,
                             void *buffer, size_t count, size_t *actual)
{
    if (!vfs || !vfs->port || !vfs->port->read_at || !actual)
        return VFS_BAD_ARG;
    *actual = 0;
    if (!client)
        return VFS_BAD_FD;

    struct vfs_client *owner = find_client(vfs, client);
    if (!owner || fd >= VFS_FILES || !owner->files[fd].used)
        return VFS_BAD_FD;
    if (count && !buffer)
        return VFS_BAD_ARG;
    if (!count)
        return VFS_OK;

    size_t n = 0;
    enum vfs_result result = vfs->port->read_at(vfs->port->ctx, owner->files[fd].inode,
                                                offset, buffer, count, &n);
    if (result != VFS_OK)
        return result;
    if (n > count)
        return VFS_IO_ERROR;
    *actual = n;
    return VFS_OK;
}

enum vfs_result vfs_close(struct vfs *vfs, uint32_t client, unsigned fd)
{
    if (!vfs || !vfs->port || !vfs->port->release)
        return VFS_BAD_ARG;
    if (!client)
        return VFS_BAD_FD;

    struct vfs_client *owner = find_client(vfs, client);
    if (!owner || fd >= VFS_FILES || !owner->files[fd].used)
        return VFS_BAD_FD;

    uint32_t inode = owner->files[fd].inode;
    owner->files[fd] = (struct vfs_file){0};
    owner->open--;
    vfs->port->release(vfs->port->ctx, inode);
    if (!owner->open)
        *owner = (struct vfs_client){0};
    return VFS_OK;
}
