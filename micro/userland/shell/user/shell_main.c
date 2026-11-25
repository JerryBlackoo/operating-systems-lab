#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"

enum { SHELL_LINE_MAX = 80 };

static int sm_lookup(uint32_t sm_send, const char *name, uint32_t sid,
                     uint32_t *handle)
{
    struct abi_message req = {
        .tag = TAG_SM_LOOKUP,
        .words = {0, svc_name_pack(name), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK && rep.words[0] != 0) {
            *handle = (uint32_t)rep.words[0];
            return 0;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

static void pack_chunk(const unsigned char *data, size_t n, uint64_t *w2,
                       uint64_t *w3)
{
    *w2 = 0;
    *w3 = 0;
    for (size_t i = 0; i < n && i < 16; i++) {
        if (i < 8) {
            *w2 |= (uint64_t)data[i] << (8U * i);
        } else {
            *w3 |= (uint64_t)data[i] << (8U * (i - 8));
        }
    }
}

static void pack24(const unsigned char *data, size_t n, uint64_t words[3])
{
    words[0] = 0;
    words[1] = 0;
    words[2] = 0;
    for (size_t i = 0; i < n && i < SVC_DATA_BYTES; i++) {
        words[i / 8U] |= (uint64_t)data[i] << (8U * (i % 8U));
    }
}

static void unpack24(uint64_t w1, uint64_t w2, uint64_t w3, unsigned char *out,
                     size_t n)
{
    uint64_t words[3] = {w1, w2, w3};

    for (size_t i = 0; i < n && i < SVC_DATA_BYTES; i++) {
        out[i] = (unsigned char)((words[i / 8U] >> (8U * (i % 8U))) & 0xffU);
    }
}

static int con_write_raw(uint32_t console, const char *data, size_t len)
{
    size_t off = 0;

    while (off < len) {
        size_t n = len - off;
        uint64_t words[3];
        struct abi_message req;
        struct abi_message rep = {0};

        if (n > SVC_DATA_BYTES) {
            n = SVC_DATA_BYTES;
        }
        pack24((const unsigned char *)data + off, n, words);
        req = (struct abi_message){
            .tag = TAG_CONSOLE_WRITE,
            .words = {n, words[0], words[1], words[2]},
        };
        if (user_ipc_call(console, &req, &rep) != ABI_OK ||
            rep.tag != TAG_CONSOLE_WRITE_OK) {
            return -1;
        }
        off += n;
    }
    return 0;
}

static int con_write_str(uint32_t console, const char *s)
{
    size_t n = 0;

    while (s[n] != '\0') {
        n++;
    }
    return con_write_raw(console, s, n);
}

static void append_u64(char *buf, size_t cap, size_t *pos, uint64_t v)
{
    char tmp[20];
    unsigned n = 0;

    if (v == 0) {
        if (*pos + 1 < cap) {
            buf[(*pos)++] = '0';
        }
        return;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10U);
        v /= 10U;
    }
    while (n != 0) {
        if (*pos + 1 < cap) {
            buf[(*pos)++] = tmp[--n];
        } else {
            n--;
        }
    }
}

static int con_read_line(uint32_t console, char *line, size_t cap, size_t *len)
{
    struct abi_message req = {.tag = TAG_CONSOLE_READ};
    struct abi_message rep = {0};
    size_t total;
    size_t n;

    if (user_ipc_call(console, &req, &rep) != ABI_OK ||
        rep.tag != TAG_CONSOLE_READ_OK) {
        return -1;
    }
    total = (size_t)rep.words[0];
    if (total > cap) {
        total = cap;
    }
    n = total;
    if (n > SVC_DATA_BYTES) {
        n = SVC_DATA_BYTES;
    }
    unpack24(rep.words[1], rep.words[2], rep.words[3], (unsigned char *)line, n);
    while (n < total) {
        size_t more;

        req = (struct abi_message){
            .tag = TAG_CONSOLE_MORE,
            .words = {n, 0, 0, 0},
        };
        rep = (struct abi_message){0};
        if (user_ipc_call(console, &req, &rep) != ABI_OK ||
            rep.tag != TAG_CONSOLE_MORE_OK) {
            return -1;
        }
        more = (size_t)rep.words[0];
        if (more > total - n) {
            more = total - n;
        }
        unpack24(rep.words[1], rep.words[2], rep.words[3],
                 (unsigned char *)line + n, more);
        n += more;
        if (more == 0) {
            break;
        }
    }
    if (total < cap) {
        line[total] = '\0';
    } else if (cap != 0) {
        line[cap - 1] = '\0';
    }
    *len = n;
    return 0;
}

static int fs_set_path(uint32_t fs, const char *path, size_t len)
{
    size_t off = 0;

    if (len == 0 || len > SVC_MAX_PATH) {
        return -1;
    }
    while (off < len) {
        size_t n = len - off;
        uint64_t w2;
        uint64_t w3;
        struct abi_message req;
        struct abi_message rep = {0};

        if (n > SVC_PATH_CHUNK) {
            n = SVC_PATH_CHUNK;
        }
        pack_chunk((const unsigned char *)path + off, n, &w2, &w3);
        req = (struct abi_message){
            .tag = TAG_FS_PATH,
            .words = {len, off, w2, w3},
        };
        if (user_ipc_call(fs, &req, &rep) != ABI_OK ||
            rep.tag != TAG_FS_PATH_OK) {
            return -1;
        }
        off += n;
    }
    return 0;
}

static void cmd_cat(uint32_t console, uint32_t fs, const char *path, size_t len)
{
    uint64_t offset = 0;

    if (fs_set_path(fs, path, len) != 0) {
        con_write_str(console, "cat: path error\n");
        return;
    }
    con_write_str(console, "----- cat ");
    con_write_raw(console, path, len);
    con_write_str(console, " -----\n");

    for (;;) {
        struct abi_message req = {
            .tag = TAG_FS_READ,
            .words = {offset, SVC_DATA_BYTES, 0, 0},
        };
        struct abi_message rep = {0};
        unsigned char data[SVC_DATA_BYTES];
        size_t n;

        if (user_ipc_call(fs, &req, &rep) != ABI_OK) {
            con_write_str(console, "cat: ipc error\n");
            break;
        }
        if (rep.tag == TAG_FS_ERROR) {
            if (rep.words[0] == (uint64_t)(int64_t)ABI_NOT_FOUND) {
                con_write_str(console, "cat: ");
                con_write_raw(console, path, len);
                con_write_str(console, ": not found\n");
            } else {
                con_write_str(console, "cat: path read error\n");
            }
            break;
        }
        if (rep.tag != TAG_FS_READ_OK) {
            con_write_str(console, "cat: bad reply\n");
            break;
        }
        n = (size_t)rep.words[0];
        if (n > SVC_DATA_BYTES) {
            n = SVC_DATA_BYTES;
        }
        unpack24(rep.words[1], rep.words[2], rep.words[3], data, n);
        if (n != 0) {
            con_write_raw(console, (const char *)data, n);
            offset += n;
        }
        if (n < SVC_DATA_BYTES) {
            break;
        }
    }
    con_write_str(console, "----- end cat -----\n");
}

static void cmd_ps(uint32_t console, uint32_t pm)
{
    char out[80];
    uint64_t index = 0;
    uint64_t total = 0;

    con_write_str(console, "ps:\n");
    for (;;) {
        struct abi_message req = {
            .tag = TAG_PM_PS,
            .words = {index, 0, 0, 0},
        };
        struct abi_message rep = {0};
        char name[SVC_NAME_BYTES + 1];
        size_t pos = 0;

        if (user_ipc_call(pm, &req, &rep) != ABI_OK || rep.tag != TAG_PM_PS_OK) {
            break;
        }
        svc_name_unpack(rep.words[1], name);
        total = rep.words[3];
        pos = 0;
        out[pos++] = 'p';
        out[pos++] = 'i';
        out[pos++] = 'd';
        out[pos++] = ' ';
        append_u64(out, sizeof(out), &pos, rep.words[0]);
        out[pos++] = ' ';
        for (unsigned i = 0; name[i] != '\0' && pos + 2 < sizeof(out); i++) {
            out[pos++] = name[i];
        }
        out[pos++] = ' ';
        append_u64(out, sizeof(out), &pos, rep.words[2]);
        out[pos++] = '\n';
        con_write_raw(console, out, pos);
        index++;
        if (index >= total) {
            break;
        }
    }
    {
        char tail[24];
        size_t pos = 0;

        tail[pos++] = 't';
        tail[pos++] = 'o';
        tail[pos++] = 't';
        tail[pos++] = 'a';
        tail[pos++] = 'l';
        tail[pos++] = ' ';
        append_u64(tail, sizeof(tail), &pos, total);
        tail[pos++] = '\n';
        con_write_raw(console, tail, pos);
    }
}

static void run_command(uint32_t console, uint32_t fs, uint32_t pm, char *line,
                        size_t len)
{
    const char *p = line;

    while (len != 0 && (line[len - 1] == '\n' || line[len - 1] == '\r')) {
        line[--len] = '\0';
    }
    while (*p == ' ' || *p == '\t') {
        p++;
    }
    if (*p == '\0') {
        return;
    }

    if (p[0] == 'h' && p[1] == 'e' && p[2] == 'l' && p[3] == 'p' && p[4] == '\0') {
        con_write_str(console, "commands: help cat ps echo poweroff\n");
    } else if (p[0] == 'c' && p[1] == 'a' && p[2] == 't' && p[3] == ' ') {
        size_t n = 0;
        const char *path = p + 4;

        while (path[n] != '\0') {
            n++;
        }
        if (n == 0) {
            con_write_str(console, "cat: missing path\n");
        } else {
            cmd_cat(console, fs, path, n);
        }
    } else if (p[0] == 'p' && p[1] == 's' && p[2] == '\0') {
        cmd_ps(console, pm);
    } else if (p[0] == 'e' && p[1] == 'c' && p[2] == 'h' && p[3] == 'o' &&
               (p[4] == '\0' || p[4] == ' ')) {
        if (p[4] == ' ') {
            con_write_str(console, p + 5);
        }
        con_write_str(console, "\n");
    } else if (p[0] == 'p' && p[1] == 'o' && p[2] == 'w' && p[3] == 'e' &&
               p[4] == 'r' && p[5] == 'o' && p[6] == 'f' && p[7] == 'f' &&
               p[8] == '\0') {
        user_poweroff();
    } else {
        con_write_str(console, "unknown: ");
        con_write_str(console, p);
        con_write_str(console, "\n");
    }
}

void shell_start(uint32_t ready_send, uint64_t receipt, uint32_t sm_send,
                 uint32_t sid) __attribute__((used));
void shell_start(uint32_t ready_send, uint64_t receipt, uint32_t sm_send,
                 uint32_t sid)
{
    uint32_t console = 0;
    uint32_t fs = 0;
    uint32_t pm = 0;
    char line[SHELL_LINE_MAX];
    size_t len;

    {
        struct abi_message ready = {
            .tag = TAG_READY,
            .words = {receipt, 0, 0, 0},
        };

        if (user_ipc_send(ready_send, &ready) != ABI_OK) {
            for (;;) {
                abi_ecall(ABI_YIELD, (struct abi_regs){0});
            }
        }
    }

    sm_lookup(sm_send, "console", sid, &console);
    sm_lookup(sm_send, "fs", sid, &fs);
    sm_lookup(sm_send, "pm", sid, &pm);

    for (;;) {
        con_write_str(console, "micro> ");
        for (;;) {
            if (con_read_line(console, line, sizeof(line), &len) == 0) {
                break;
            }
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
        }
        run_command(console, fs, pm, line, len);
    }
}