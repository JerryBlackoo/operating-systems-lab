#ifndef MICRO_USERLAND_SERVICE_PROTO_H
#define MICRO_USERLAND_SERVICE_PROTO_H

#include <stdint.h>

/*
 * SID 与 IPC tag 的**权威定义在 shared/abi/service.h**。
 *
 * 历史上这里有一份独立副本，结果是新增 tag（TAG_VM_RELEASE / TAG_VM_FORGET）
 * 时必须记得改两处，而 SID 只在用户态有一份、内核侧另有一份 `SERVICE_SID_*`。
 * 现在统一由 ABI 头提供，本文件只保留用户态专用的字符串打包辅助函数。
 */
#include "abi/service.h"

enum service_const {
    SVC_SPAWN_OP = 1,
    SVC_NAME_BYTES = 8,
    SVC_DATA_BYTES = 24,
    SVC_PATH_CHUNK = 16,
    SVC_CAP_WRITE = 2,
    SVC_MAX_PATH = 255,
};

static inline uint64_t svc_name_pack(const char *s)
{
    uint64_t v = 0;

    for (unsigned i = 0; i < SVC_NAME_BYTES && s[i] != '\0'; i++) {
        v |= (uint64_t)(uint8_t)s[i] << (8U * i);
    }
    return v;
}

static inline void svc_name_unpack(uint64_t v, char out[SVC_NAME_BYTES + 1])
{
    for (unsigned i = 0; i < SVC_NAME_BYTES; i++) {
        out[i] = (char)((v >> (8U * i)) & 0xffU);
    }
    out[SVC_NAME_BYTES] = '\0';
}

static inline int svc_name_equals(uint64_t v, const char *s)
{
    char name[SVC_NAME_BYTES + 1];

    svc_name_unpack(v, name);
    for (unsigned i = 0; i <= SVC_NAME_BYTES; i++) {
        if (name[i] != s[i]) {
            return 0;
        }
        if (s[i] == '\0') {
            return 1;
        }
    }
    return 0;
}

static inline uint64_t svc_pack8(const unsigned char bytes[SVC_NAME_BYTES])
{
    uint64_t v = 0;

    for (unsigned i = 0; i < SVC_NAME_BYTES; i++) {
        v |= (uint64_t)bytes[i] << (8U * i);
    }
    return v;
}

#endif
