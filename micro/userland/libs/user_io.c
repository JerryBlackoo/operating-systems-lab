#include <stdint.h>

enum { DEMO_PRINT = 0x10000 };

void user_print(const char *line)
{
#if defined(__riscv)
    register uint64_t a0 __asm__("a0") = (uint64_t)(uintptr_t)line;
    register uint64_t a7 __asm__("a7") = DEMO_PRINT;

    __asm__ volatile("ecall" : "+r"(a0) : "r"(a7) : "memory");
#else
    (void)line;
#endif
}

void user_print_u64(const char *prefix, uint64_t v, const char *suffix)
{
    /*
     * 最长现有调用前缀是 "[init] spawn failed status=-"（28 B），
     * 加 20 位十进制和 NUL 已超过旧的 48 B。统一扩容并做边界截断。
     */
    char buf[96];
    char *p = buf;
    char *const end = buf + sizeof(buf) - 1;
    const char *s;

    for (s = prefix; *s && p < end; s++) {
        *p++ = *s;
    }
    if (v == 0) {
        if (p < end) {
            *p++ = '0';
        }
    } else {
        char tmp[20];
        unsigned n = 0;

        while (v && n < sizeof(tmp)) {
            tmp[n++] = (char)('0' + v % 10);
            v /= 10;
        }
        while (n && p < end) {
            *p++ = tmp[--n];
        }
    }
    for (s = suffix; *s && p < end; s++) {
        *p++ = *s;
    }
    *p = '\0';
    user_print(buf);
}