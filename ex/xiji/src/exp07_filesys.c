/* 希冀实验七：树形文件系统 mkdir/create/read/write/del/rm */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define DATA_MAX 64

struct fs_node {
    char name[16];
    int isdir;
    char data[DATA_MAX];
    int data_len;
    struct fs_node *parent;
    struct fs_node *child;
    struct fs_node *next;
    struct fs_node *prev;
};

static struct fs_node pool[24];
static int pool_used;
static struct fs_node *root;
static struct fs_node *recent;
static struct fs_node *open_file;

static struct fs_node *node_new(const char *name, int isdir, struct fs_node *parent)
{
    if (pool_used >= 24)
        return 0;
    struct fs_node *n = &pool[pool_used++];
    for (int i = 0; i < 16; i++)
        n->name[i] = name[i] ? name[i] : '\0';
    n->isdir = isdir;
    n->data_len = 0;
    n->parent = parent;
    n->child = n->next = n->prev = 0;
    return n;
}

static struct fs_node *lookup_child(struct fs_node *dir, const char *name)
{
    for (struct fs_node *c = dir->child; c; c = c->next) {
        int match = 1;
        for (int i = 0; name[i] || c->name[i]; i++) {
            if (name[i] != c->name[i]) {
                match = 0;
                break;
            }
        }
        if (match)
            return c;
    }
    return 0;
}

static int attach_child(struct fs_node *parent, struct fs_node *child)
{
    child->parent = parent;
    if (!parent->child) {
        parent->child = child;
        return 0;
    }
    struct fs_node *tail = parent->child;
    while (tail->next)
        tail = tail->next;
    tail->next = child;
    child->prev = tail;
    return 0;
}

static int mkdir(const char *name)
{
    if (!recent || !recent->isdir)
        return -1;
    if (lookup_child(recent, name))
        return -2;
    struct fs_node *d = node_new(name, 1, recent);
    if (!d)
        return -3;
    attach_child(recent, d);
    return 0;
}

static int create(const char *name)
{
    if (!recent || !recent->isdir)
        return -1;
    if (lookup_child(recent, name))
        return -2;
    struct fs_node *f = node_new(name, 0, recent);
    if (!f)
        return -3;
    attach_child(recent, f);
    return 0;
}

static int open(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);
    if (!n || n->isdir)
        return -1;
    open_file = n;
    return 0;
}

static void close_fs(void)
{
    open_file = 0;
}

static int write_file(const char *payload)
{
    if (!open_file)
        return -1;
    int len = 0;
  while (payload[len] && len < DATA_MAX - 1)
        len++;
    for (int i = 0; i < len; i++)
        open_file->data[i] = payload[i];
    open_file->data_len = len;
    open_file->data[len] = '\0';
    return len;
}

static int read_file(char *out, int cap)
{
    if (!open_file)
        return -1;
    int n = open_file->data_len;
    if (n >= cap)
        n = cap - 1;
    for (int i = 0; i < n; i++)
        out[i] = open_file->data[i];
    out[n] = '\0';
    return n;
}

static int del_file(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);
    if (!n || n->isdir)
        return -1;
    if (n->prev)
        n->prev->next = n->next;
    else if (recent->child == n)
        recent->child = n->next;
    if (n->next)
        n->next->prev = n->prev;
    return 0;
}

static int rm_dir(const char *name)
{
    struct fs_node *n = lookup_child(recent, name);
    if (!n || !n->isdir)
        return -1;
    if (n->child)
        return -2;
    if (n->prev)
        n->prev->next = n->next;
    else if (recent->child == n)
        recent->child = n->next;
    if (n->next)
        n->next->prev = n->prev;
    return 0;
}

int xiji_exp07_run(void)
{
    pool_used = 0;
    root = node_new("/", 1, 0);
    recent = root;

    XIJI_CHECK("exp07-mkdir", mkdir("docs") == 0);
    struct fs_node *docs = lookup_child(root, "docs");
    XIJI_CHECK("exp07-mkdir", docs != 0);

    recent = docs;
    XIJI_CHECK("exp07-create", create("readme.txt") == 0);
    XIJI_CHECK("exp07-dup", create("readme.txt") == -2);

    XIJI_CHECK("exp07-open", open("readme.txt") == 0);
    XIJI_CHECK("exp07-write", write_file("hello-xiji") == 10);

    char buf[32];
    XIJI_CHECK("exp07-read", read_file(buf, sizeof buf) == 10);
    XIJI_CHECK("exp07-read-data", buf[0] == 'h' && buf[6] == 'x');

    close_fs();
    XIJI_CHECK("exp07-del", del_file("readme.txt") == 0);
    XIJI_CHECK("exp07-mkdir2", mkdir("sub") == 0);
    recent = lookup_child(root, "docs")->child;
    XIJI_CHECK("exp07-inner", create("inner.txt") == 0);
    recent = lookup_child(root, "docs");
    XIJI_CHECK("exp07-rm-busy", rm_dir("sub") == -2);
    recent = lookup_child(recent, "sub");
    XIJI_CHECK("exp07-del-inner", del_file("inner.txt") == 0);
    recent = lookup_child(root, "docs");
    XIJI_CHECK("exp07-rm", rm_dir("sub") == 0);

    xiji_puts("PASS exp07-file-management\n");
    return 0;
}