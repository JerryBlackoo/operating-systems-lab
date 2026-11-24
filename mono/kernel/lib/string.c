#include "lib/string.h"

void *memset(void *dst, int value, uint64_t len)
{
    unsigned char *p = (unsigned char *)dst;

    while (len-- != 0) {
        *p++ = (unsigned char)value;
    }
    return dst;
}

void *memcpy(void *dst, const void *src, uint64_t len)
{
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;

    while (len-- != 0) {
        *d++ = *s++;
    }
    return dst;
}

void *memmove(void *dst, const void *src, uint64_t len)
{
    unsigned char *d = (unsigned char *)dst;
    const unsigned char *s = (const unsigned char *)src;

    if (d < s) {
        while (len-- != 0) {
            *d++ = *s++;
        }
    } else if (d > s) {
        d += len;
        s += len;
        while (len-- != 0) {
            *--d = *--s;
        }
    }
    return dst;
}

int memcmp(const void *a, const void *b, uint64_t len)
{
    const unsigned char *x = (const unsigned char *)a;
    const unsigned char *y = (const unsigned char *)b;

    for (uint64_t i = 0; i < len; i++) {
        if (x[i] != y[i]) {
            return (int)x[i] - (int)y[i];
        }
    }
    return 0;
}

uint64_t strlen(const char *s)
{
    uint64_t n = 0;

    while (s[n] != '\0') {
        n++;
    }
    return n;
}

int strcmp(const char *a, const char *b)
{
    while (*a != '\0' && *a == *b) {
        a++;
        b++;
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

int strncmp(const char *a, const char *b, uint64_t n)
{
    while (n != 0 && *a != '\0' && *a == *b) {
        a++;
        b++;
        n--;
    }
    if (n == 0) {
        return 0;
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

char *strncpy(char *dst, const char *src, uint64_t n)
{
    uint64_t i = 0;

    for (; i < n && src[i] != '\0'; i++) {
        dst[i] = src[i];
    }
    for (; i < n; i++) {
        dst[i] = '\0';
    }
    return dst;
}

char *strcpy(char *dst, const char *src)
{
    char *out = dst;

    while ((*dst++ = *src++) != '\0') {
    }
    return out;
}
