import struct

# Verify the patch is in the image
img = r'd:\my\STUDY\university\U_3\up\OS\match\code\RustMicroOS\sdcard-rv.img'
with open(img, 'rb') as f:
    for off in [0x85d9000, 0x555c3000]:
        f.seek(off + 0x4B0)
        code = f.read(20)
        print(f'Image 0x{off:x}+0x4B0: {code.hex()}')
        if code[:4] == bytes.fromhex('17270000'):
            print('  -> PATCHED (auipc a4, 0x2)')
        else:
            print('  -> ORIGINAL (not patched!)')

# Read the DSO
with open(img, 'rb') as f:
    f.seek(0x85d9000)
    dso = f.read(5376)

# Parse relocations
rela_off = 0x310
rela_size = 0xD8
rela_count = rela_size // 24
dynsym_off = 0x1B8
dynstr_off = 0x290

print(f'\nRelocations ({rela_count} entries):')
for i in range(rela_count):
    base = rela_off + i * 24
    r_offset = struct.unpack_from('<Q', dso, base)[0]
    r_info = struct.unpack_from('<Q', dso, base + 8)[0]
    r_addend = struct.unpack_from('<q', dso, base + 16)[0]
    r_sym = r_info >> 32
    r_type = r_info & 0xFFFFFFFF
    
    type_name = {0: 'NONE', 2: 'R_RISCV_64', 3: 'R_RISCV_RELATIVE'}.get(r_type, f'type={r_type}')
    
    sym_name = ''
    if r_sym > 0:
        sym_base = dynsym_off + r_sym * 24
        st_name = struct.unpack_from('<I', dso, sym_base)[0]
        end = dso.index(b'\0', dynstr_off + st_name)
        sym_name = dso[dynstr_off + st_name:end].decode('ascii', errors='replace')
    
    print(f'  Rela[{i}]: offset=0x{r_offset:04x} type={type_name} sym="{sym_name}" addend=0x{r_addend & 0xFFFFFFFFFFFFFFFF:x}')

# Show GOT contents (raw bytes at file offsets)
print('\nGOT raw bytes (at file offset 0xEB0 + (0x2010 - 0x1EB0)):')
# Data segment: file offset 0xEB0 maps to vaddr 0x1EB0
# GOT starts at vaddr 0x2010, so file offset = 0xEB0 + (0x2010 - 0x1EB0) = 0xEB0 + 0x160 = 0x1010
got_file_off = 0xEB0 + (0x2010 - 0x1EB0)
for entry_idx in range(9):
    off_in_file = got_file_off + entry_idx * 8
    val = struct.unpack_from('<Q', dso, off_in_file)[0]
    vaddr = 0x2010 + entry_idx * 8
    print(f'  GOT[{entry_idx}] vaddr=0x{vaddr:04x} file_off=0x{off_in_file:04x} raw=0x{val:016x}')
