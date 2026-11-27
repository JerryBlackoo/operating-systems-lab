#ifndef XIJI_HW_PAGING_H
#define XIJI_HW_PAGING_H

#include <stdint.h>

struct paging_stats {
    uint32_t faults;
    uint32_t frames_used;
};

void hw_paging_on_fault(uint32_t cr2, uint32_t err_code);
int hw_paging_init(void);
int hw_paging_demand_test(struct paging_stats *out);

#endif