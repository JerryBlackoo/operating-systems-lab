
#!/usr/bin/env python3
from pathlib import Path
import re, sys
ROOT = Path(__file__).resolve().parents[1]

def read(rel): return (ROOT / rel).read_text(encoding='utf-8')

def norm(s): return re.sub(r'\s+', ' ', s)

def find_matching_brace(src, open_idx):
    depth=0; i=open_idx; state='code'; n=len(src)
    while i<n:
        c=src[i]; nxt=src[i+1] if i+1<n else ''
        if state=='code':
            if c=='/' and nxt=='/': state='line'; i+=2; continue
            if c=='/' and nxt=='*': state='block'; i+=2; continue
            if c=='"': state='str'; i+=1; continue
            if c=="'": state='char'; i+=1; continue
            if c=='{': depth+=1
            elif c=='}':
                depth-=1
                if depth==0: return i
            i+=1; continue
        if state=='line':
            if c=='\n': state='code'
            i+=1; continue
        if state=='block':
            if c=='*' and nxt=='/': state='code'; i+=2; continue
            i+=1; continue
        if state=='str':
            if c=='\\': i+=2; continue
            if c=='"': state='code'
            i+=1; continue
        if state=='char':
            if c=='\\': i+=2; continue
            if c=="'": state='code'
            i+=1; continue
    raise RuntimeError('unmatched brace')

def fn_body(src, name):
    m=re.search(r'\b(?:pub\s+)?(?:unsafe\s+)?fn\s+'+re.escape(name)+r'\b', src)
    if not m: return ''
    o=src.find('{', m.end())
    if o<0: return ''
    c=find_matching_brace(src,o)
    return src[o+1:c]

def cfg_block(src, func, cfg):
    b=fn_body(src, func)
    if not b: return ''
    i=b.find(cfg)
    if i<0: return ''
    o=b.find('{', i)
    if o<0: return ''
    c=find_matching_brace(b,o)
    return b[o+1:c]

checks=[]
def check(ok,msg): checks.append((ok,msg))
def finish():
    p=sum(1 for ok,_ in checks if ok); f=len(checks)-p
    for ok,msg in checks:
        print(('PASS' if ok else 'FAIL')+': '+msg)
    print(f'SUMMARY: PASS={p} FAIL={f}')
    sys.exit(0 if f==0 else 1)

mem=read('kernel/src/memory.rs')
rv=cfg_block(mem, 'initialize_page_allocator', '#[cfg(target_arch = "riscv64")]')
check('install_boot(page_allocator)' in rv, 'RISC-V has a boot install point')
check('is_initialized_boot' not in rv, 'RISC-V does not reread boot allocator after install')
check('is_initialized()' not in rv, 'RISC-V does not enter runtime init check')
check('riscv_boot_puts' in rv and 'early handoff: complete' in rv, 'RISC-V reports completion through boot helper')
check('riscv page_alloc install_boot:' not in mem, 'temporary install_boot trace removed')
finish()
