#include "userland/lib.h"

static void prompt(void)
{
    os_puts("sh> ");
}

int main(void)
{
    char line[128];

    prompt();
    for (;;) {
        long n = os_read(0, line, sizeof(line) - 1);
        char *cmd;
        char *arg;
        char *p;

        if (n <= 0) {
            continue;
        }
        line[n] = '\0';
        while (n > 0 && (line[n - 1] == '\n' || line[n - 1] == '\r')) {
            line[--n] = '\0';
        }

        cmd = line;
        while (*cmd == ' ') {
            cmd++;
        }
        arg = cmd;
        while (*arg != '\0' && *arg != ' ') {
            arg++;
        }
        if (*arg != '\0') {
            *arg++ = '\0';
            while (*arg == ' ') {
                arg++;
            }
        }

        if (*cmd == '\0') {
            prompt();
            continue;
        }

        if (os_strcmp(cmd, "help") == 0) {
            os_puts("commands: help ps cat echo fork_test mkdir exit\n");
        } else if (os_strcmp(cmd, "ps") == 0) {
            os_ps();
        } else if (os_strcmp(cmd, "cat") == 0) {
            long fd = os_open(arg, 0);
            if (fd < 0) {
                os_puts("cat: cannot open\n");
            } else {
                char buf[128];
                long r;
                while ((r = os_read(fd, buf, sizeof(buf))) > 0) {
                    os_write(1, buf, (unsigned long)r);
                }
                os_close(fd);
            }
        } else if (os_strcmp(cmd, "echo") == 0) {
            os_puts(arg);
            os_puts("\n");
        } else if (os_strcmp(cmd, "fork_test") == 0) {
            long pid = os_fork();
            if (pid == 0) {
                os_puts("fork_test: child\n");
                os_exit(0);
            } else if (pid > 0) {
                os_puts("fork_test: parent\n");
            } else {
                os_puts("fork_test: failed\n");
            }
        } else if (os_strcmp(cmd, "mkdir") == 0) {
            os_puts(os_mkdir(arg) == 0 ? "mkdir: ok\n" : "mkdir: failed\n");
        } else if (os_strcmp(cmd, "exit") == 0) {
            return 0;
        } else {
            os_puts("sh: unknown command\n");
        }

        prompt();
        p = line;
        (void)p;
    }
}
