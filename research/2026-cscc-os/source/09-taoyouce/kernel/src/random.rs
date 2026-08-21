use crate::sync::SpinLock;

struct RandomState {
    seeded: bool,
    state: u64,
}

static RANDOM_STATE: SpinLock<RandomState> = SpinLock::new(RandomState {
    seeded: false,
    state: 0,
});

pub fn init() {
    let seed = crate::arch::asm_helpers::rdtime()
        ^ ((crate::arch::asm_helpers::read_cpu_id() as u64) << 32)
        ^ 0x9e37_79b9_7f4a_7c15;
    reseed(seed);
}

pub fn reseed(seed: u64) {
    let mut state = RANDOM_STATE.lock();
    state.seeded = true;
    state.state ^= mix(seed);
    if state.state == 0 {
        state.state = 0xa5a5_5a5a_d3c1_b2e7;
    }
}

pub fn fill_bytes(buf: &mut [u8]) {
    let mut state = RANDOM_STATE.lock();
    if !state.seeded {
        let seed = crate::arch::asm_helpers::rdtime()
            ^ ((crate::arch::asm_helpers::read_cpu_id() as u64) << 32)
            ^ 0x243f_6a88_85a3_08d3;
        state.seeded = true;
        state.state = mix(seed);
        if state.state == 0 {
            state.state = 0x1319_8a2e_0370_7344;
        }
    }

    let mut word = 0u64;
    let mut remaining = 0usize;
    for byte in buf.iter_mut() {
        if remaining == 0 {
            state.state = next(state.state);
            word = state.state;
            remaining = 8;
        }
        *byte = (word & 0xff) as u8;
        word >>= 8;
        remaining -= 1;
    }
}

fn next(mut value: u64) -> u64 {
    value ^= value << 7;
    value ^= value >> 9;
    value = value.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    value ^ crate::arch::asm_helpers::rdtime()
}

fn mix(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    value ^ (value >> 33)
}
