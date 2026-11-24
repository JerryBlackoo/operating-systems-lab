#include "lib.h"

unsigned long os_strlen(const char *s)
{
    unsigned long n = 0;

    while (s[n] != '\0') {
        n++;
    }
    return n;
}

int os_strcmp(const char *a, const char *b)
{
    while (*a != '\0' && *a == *b) {
        a++;
        b++;
    }
    return (int)(unsigned char)*a - (int)(unsigned char)*b;
}

int os_strncmp(const char *a, const char *b, unsigned long n)
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

void os_puts(const char *s)
{
    os_write(1, s, os_strlen(s));
}

void os_putchar(char c)
{
    os_write(1, &c, 1);
}
