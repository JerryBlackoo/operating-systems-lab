#include "fs/disk_sched.h"

#include "lib/kprintf.h"

#define CYL_MAX 200

static void scan_once(int *req, int n, int head, int upward, int *order, int *count)
{
    if (upward) {
        for (int c = head; c < CYL_MAX; c++) {
            for (int i = 0; i < n; i++) {
                if (req[i] == c) {
                    order[(*count)++] = req[i];
                }
            }
        }
        for (int c = head - 1; c >= 0; c--) {
            for (int i = 0; i < n; i++) {
                if (req[i] == c) {
                    order[(*count)++] = req[i];
                }
            }
        }
    }
}

void disk_sched_selftest(void)
{
    int requests[] = {98, 183, 37, 122, 14, 124, 65, 67};
    int n = (int)(sizeof(requests) / sizeof(requests[0]));
    int order[16];
    int count = 0;

    scan_once(requests, n, 53, 1, order, &count);
    kprintf("[disk] SCAN from 53 (up):");
    for (int i = 0; i < count; i++) {
        kprintf(" %d", order[i]);
    }
    kprintf("\n");
}
