#include "vfs.h"

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
        enum vfs_result result = vfs->port->lookup(vfs->port->ctx, node->inode, name, len, node);
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
