#ifndef MICRO_USERLAND_USER_IO_H
#define MICRO_USERLAND_USER_IO_H

#include <stdint.h>

void user_print(const char *line);
void user_print_u64(const char *prefix, uint64_t v, const char *suffix);
void user_print_name_u64(const char *prefix, const char *name, const char *mid,
                         uint64_t v, const char *suffix);

#endif