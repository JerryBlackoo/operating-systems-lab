#include <stdint.h>

/* ARM 32 位没有 64 位除法指令，编译器会调用 RTABI 的 __aeabi_* 辅助函数。
 * RTABI 要求商/余数直接放在 r0-r3，普通 C 的 struct 返回约定无法表达，
 * 因此真正的 __aeabi_* 入口在 eabi.S 中；这里只提供普通 AAPCS 的算法。 */

struct u64_divmod {
    uint64_t quot;
    uint64_t rem;
};

struct u32_divmod {
    uint32_t quot;
    uint32_t rem;
};

void arm_uldivmod_c(uint64_t numerator, uint64_t denominator, struct u64_divmod *out)
{
    struct u64_divmod result = {0, 0};

    if (denominator != 0) {
        for (int i = 63; i >= 0; i--) {
            uint64_t carry = result.rem >> 63;
            result.rem = (result.rem << 1) | ((numerator >> i) & 1u);
            if (carry != 0 || result.rem >= denominator) {
                result.rem -= denominator;
                result.quot |= (uint64_t)1 << i;
            }
        }
    }
    *out = result;
}

void arm_uidivmod_c(uint32_t numerator, uint32_t denominator, struct u32_divmod *out)
{
    struct u32_divmod result = {0, 0};

    if (denominator != 0) {
        for (int i = 31; i >= 0; i--) {
            uint32_t carry = result.rem >> 31;
            result.rem = (result.rem << 1) | ((numerator >> i) & 1u);
            if (carry != 0 || result.rem >= denominator) {
                result.rem -= denominator;
                result.quot |= 1u << i;
            }
        }
    }
    *out = result;
}

uint32_t __aeabi_uidiv(uint32_t numerator, uint32_t denominator)
{
    struct u32_divmod result;

    arm_uidivmod_c(numerator, denominator, &result);
    return result.quot;
}
