#include "sync/banker.h"

#include "lib/kprintf.h"
#include "lib/string.h"

int banker_safety_check(const struct banker_state *state, int *safe_seq)
{
    int work[BANKER_MAX_RES];
    int finish[BANKER_MAX_PROC];
    int count = 0;

    for (int i = 0; i < state->nproc; i++) {
        finish[i] = 0;
    }
    for (int r = 0; r < state->nres; r++) {
        work[r] = state->available[r];
    }

    while (count < state->nproc) {
        int found = -1;

        for (int p = 0; p < state->nproc; p++) {
            int can = 1;
            if (finish[p] != 0) {
                continue;
            }
            for (int r = 0; r < state->nres; r++) {
                if (state->need[p][r] > work[r]) {
                    can = 0;
                    break;
                }
            }
            if (can != 0) {
                found = p;
                break;
            }
        }

        if (found < 0) {
            return 0;
        }

        for (int r = 0; r < state->nres; r++) {
            work[r] += state->allocation[found][r];
        }
        finish[found] = 1;
        safe_seq[count++] = found;
    }
    return count;
}

void banker_selftest(void)
{
    static const int available[3] = {3, 3, 2};
    static const int max[5][3] = {
        {7, 5, 3},
        {3, 2, 2},
        {9, 0, 2},
        {2, 2, 2},
        {4, 3, 3},
    };
    static const int allocation[5][3] = {
        {0, 1, 0},
        {2, 0, 0},
        {3, 0, 2},
        {2, 1, 1},
        {0, 0, 2},
    };
    struct banker_state state;
    int seq[BANKER_MAX_PROC];
    int n;

    memset(&state, 0, sizeof(state));
    state.nproc = 5;
    state.nres = 3;
    for (int r = 0; r < 3; r++) {
        state.available[r] = available[r];
    }
    for (int p = 0; p < 5; p++) {
        for (int r = 0; r < 3; r++) {
            state.max[p][r] = max[p][r];
            state.allocation[p][r] = allocation[p][r];
            state.need[p][r] = max[p][r] - allocation[p][r];
        }
    }

    n = banker_safety_check(&state, seq);
    if (n == 0) {
        kprintf("[banker] unsafe state\n");
        return;
    }

    kprintf("[banker] safe seq:");
    for (int i = 0; i < n; i++) {
        kprintf(" P%d", seq[i]);
    }
    kprintf("\n");
}
