#!/usr/bin/env python3
"""
Offline analysis of QEMU MMU/page-table traces to find premature frame deallocation.

Strategy:
  1. Parse QEMU `-d mmu,page,int` trace to extract:
     - Page table map/unmap events (PPN <-> VPN associations)
     - Page faults and their resolution
     - The instruction-fetch fault that causes the [ILL]
  2. Parse kernel serial output to extract:
     - [ILL] crash info (vpn, ppn, pid)
     - alloc_frame / dealloc_frame events (if kernel logging enabled)
  3. Cross-reference: find the PPN that WAS mapped at the faulting VPN,
     trace when it was unmapped, and what instruction caused the unmap.

Usage:
  python3 tools/analyze_mmu_trace.py <trace.log> <serial.log> [kernel-rv ELF]

The trace.log comes from: qemu-system-riscv64 ... -d mmu,page -D trace.log
The serial.log comes from: redirecting QEMU stdout
The ELF is optional for addr2line resolution.
"""

import re
import sys
import subprocess
from collections import defaultdict


def parse_serial_ill(serial_path):
    """Extract [ILL] crash info from serial output."""
    ills = []
    with open(serial_path) as f:
        for line in f:
            m = re.search(r'\[ILL\].*vpn=0x([0-9a-f]+).*ppn=(\w+)', line)
            if m:
                vpn = int(m.group(1), 16)
                ppn_str = m.group(2)
                ppn = int(ppn_str, 16) if ppn_str != 'None' else None
                ills.append({'vpn': vpn, 'ppn': ppn, 'line': line.strip()})
    return ills


def parse_qemu_mmu_trace(trace_path, target_vpn=None):
    """
    Parse QEMU MMU trace for page table operations.
    
    QEMU `-d mmu` outputs lines like:
      riscv_cpu_tlb_fill: addr=0x34850 mode=0 mmu_idx=0
      ...
      
    QEMU `-d page` outputs page table dumps.
    """
    events = []
    
    # Patterns for RISC-V QEMU MMU trace
    patterns = {
        'tlb_fill': re.compile(
            r'riscv_cpu_tlb_fill.*addr=(0x[0-9a-f]+).*ret=(\d+)'
        ),
        'page_fault': re.compile(
            r'(store|load|fetch)_page_fault.*addr=(0x[0-9a-f]+)'
        ),
        'pte_update': re.compile(
            r'PTE.*va=(0x[0-9a-f]+).*pa=(0x[0-9a-f]+)'
        ),
        # Generic page table walk
        'walk': re.compile(
            r'walk.*va=(0x[0-9a-f]+).*pa=(0x[0-9a-f]+)'
        ),
    }
    
    line_no = 0
    with open(trace_path) as f:
        for line in f:
            line_no += 1
            for name, pat in patterns.items():
                m = pat.search(line)
                if m:
                    evt = {'type': name, 'line_no': line_no, 'raw': line.strip()}
                    if name == 'tlb_fill':
                        addr = int(m.group(1), 16)
                        vpn = addr >> 12
                        evt['vpn'] = vpn
                        evt['addr'] = addr
                        evt['ret'] = int(m.group(2))
                    elif name in ('page_fault', 'pte_update', 'walk'):
                        addr = int(m.group(2) if name == 'page_fault' else m.group(1), 16)
                        vpn = addr >> 12
                        evt['vpn'] = vpn
                        evt['addr'] = addr
                    
                    if target_vpn is None or evt.get('vpn') == target_vpn:
                        events.append(evt)
                    break
    
    return events


def addr2line(elf_path, addrs):
    """Resolve addresses to file:line using addr2line."""
    if not elf_path or not addrs:
        return {}
    try:
        addr_strs = [f'0x{a:x}' for a in addrs]
        result = subprocess.run(
            ['riscv64-unknown-elf-addr2line', '-e', elf_path, '-f', '-p'] + addr_strs,
            capture_output=True, text=True, timeout=10
        )
        lines = result.stdout.strip().split('\n')
        mapping = {}
        for addr, line in zip(addrs, lines):
            mapping[addr] = line.strip()
        return mapping
    except Exception as e:
        print(f"[warn] addr2line failed: {e}")
        return {}


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(1)
    
    trace_path = sys.argv[1]
    serial_path = sys.argv[2]
    elf_path = sys.argv[3] if len(sys.argv) > 3 else None
    
    print("=" * 60)
    print("RustMicroOS MMU Trace Analyzer")
    print("=" * 60)
    
    # 1. Parse serial for [ILL] crashes
    ills = parse_serial_ill(serial_path)
    if not ills:
        print("[!] No [ILL] crashes found in serial output.")
        print("    The bug may not have been triggered in this run.")
        sys.exit(0)
    
    for ill in ills:
        print(f"\n[ILL CRASH] {ill['line']}")
        target_vpn = ill['vpn']
        print(f"  Target VPN: {target_vpn:#x} (VA: {target_vpn << 12:#x})")
        
        # 2. Parse QEMU trace for events related to this VPN
        print(f"\n  Searching trace for vpn={target_vpn:#x}...")
        events = parse_qemu_mmu_trace(trace_path, target_vpn)
        
        if not events:
            print(f"  [!] No MMU events found for vpn={target_vpn:#x}")
            print(f"      Try running with: qemu-system-riscv64 -d mmu,page,guest_errors -D trace.log")
        else:
            print(f"  Found {len(events)} events for vpn={target_vpn:#x}:")
            for evt in events[-20:]:  # Last 20 events
                print(f"    [{evt['line_no']:>8}] {evt['type']:>15}: {evt['raw'][:120]}")
    
    # 3. Also look for any page fault patterns near the crash
    print("\n\n--- Searching for ALL page faults near crash ---")
    all_faults = []
    with open(trace_path) as f:
        for i, line in enumerate(f, 1):
            if 'page_fault' in line.lower() or 'exception' in line.lower():
                all_faults.append((i, line.strip()))
    
    if all_faults:
        print(f"  Found {len(all_faults)} fault events. Last 20:")
        for line_no, line in all_faults[-20:]:
            print(f"    [{line_no:>8}] {line[:120]}")
    else:
        print("  No page_fault events in trace (QEMU may need -d guest_errors,mmu)")
    
    print("\n" + "=" * 60)
    print("ANALYSIS COMPLETE")
    print("=" * 60)


if __name__ == '__main__':
    main()
