#ifndef KERNEL_LIB_STRING_H
#define KERNEL_LIB_STRING_H

#include <stdint.h>

void *memset(void *dst, int value, uint64_t len);
void *memcpy(void *dst, const void *src, uint64_t len);
void *memmove(void *dst, const void *src, uint64_t len);
int memcmp(const void *a, const void *b, uint64_t len);
uint64_t strlen(const char *s);
int strcmp(const char *a, const char *b);
int strncmp(const char *a, const char *b, uint64_t n);
char *strncpy(char *dst, const char *src, uint64_t n);
char *strcpy(char *dst, const char *src);

#endif
