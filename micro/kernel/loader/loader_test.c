#define _POSIX_C_SOURCE 200112L

#include "loader.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "elf.h"
#include "object.h"

#define TEST_BASE 0x80000000ULL
#define TEST_SIZE (8ULL * 1024 * 1024)
#define IMAGE_SIZE 16384
#define USER_VA 0x40000000ULL

void demo_log(const char *line)
{
    (void)line;
}

static void put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)v;
    p[1] = (uint8_t)(v >> 8);
    p[2] = (uint8_t)(v >> 16);
    p[3] = (uint8_t)(v >> 24);
}

static size_t make_elf(uint8_t *img)
{
    struct elf64_ehdr eh = {0};
    struct elf64_phdr ph[2] = {0};

    memset(img, 0, IMAGE_SIZE);
    eh.e_ident[0] = ELFMAG0;
    eh.e_ident[1] = ELFMAG1;
    eh.e_ident[2] = ELFMAG2;
    eh.e_ident[3] = ELFMAG3;
    eh.e_ident[4] = 2;
    eh.e_ident[5] = 1;
    eh.e_type = ET_EXEC;
    eh.e_machine = EM_RISCV;
    eh.e_entry = USER_VA + 0x204;
    eh.e_phoff = sizeof(eh);
    eh.e_ehsize = sizeof(eh);
    eh.e_phentsize = sizeof(ph[0]);
    eh.e_phnum = 2;

    ph[0].p_type = PT_LOAD;
    ph[0].p_flags = PF_R | PF_X;
    ph[0].p_offset = 0x200;
    ph[0].p_vaddr = USER_VA + 0x200;
    ph[0].p_filesz = 32;
    ph[0].p_memsz = 128;
    ph[0].p_align = 16;

    ph[1].p_type = PT_LOAD;
    ph[1].p_flags = PF_R;
    ph[1].p_offset = 0x220;
    ph[1].p_vaddr = USER_VA + 0x220;
    ph[1].p_filesz = 16;
    ph[1].p_memsz = 16;
    ph[1].p_align = 16;

    memcpy(img, &eh, sizeof(eh));
    memcpy(img + sizeof(eh), ph, sizeof(ph));
    for (unsigned i = 0; i < 32; i++) {
        img[0x200 + i] = (uint8_t)(0x40 + i);
    }
    for (unsigned i = 0; i < 16; i++) {
        img[0x220 + i] = (uint8_t)(0x80 + i);
    }
    return sizeof(eh) + sizeof(ph) + 0x300;
}

static struct as *new_as(uint64_t *before)
{
    struct as *as = as_create();

    assert(as != NULL);
    *before = frame_free_count();
    return as;
}

static void test_shared_partial_page_and_bss(uint8_t *img)
{
    uint64_t before;
    struct as *as = new_as(&before);
    struct loader_image out;
    pfn_t pfn;
    uint8_t *page;
    int rights;

    assert(loader_load_elf(as, img, IMAGE_SIZE, &out) == MEM_OK);
    assert(out.entry == USER_VA + 0x204);
    assert(as_translate(as, USER_VA + 0x200, &rights, &pfn) == MEM_OK);
    assert((rights & (MEM_R | MEM_X | MEM_U)) == (MEM_R | MEM_X | MEM_U));
    assert((rights & MEM_W) == 0);
    page = frame_ptr(pfn);
    for (unsigned i = 0; i < 32; i++) {
        assert(page[0x200 + i] == (uint8_t)(0x40 + i));
    }
    for (unsigned i = 0; i < 16; i++) {
        assert(page[0x220 + i] == (uint8_t)(0x80 + i));
    }
    for (unsigned i = 0x230; i < 0x280; i++) {
        assert(page[i] == 0);
    }
    assert(as_translate(as, USER_VA + PAGE_SIZE, &rights, &pfn) == MEM_EFAULT);
    as_unref(as);
    assert(frame_free_count() == before + 1);
    printf("loader: partial/shared pages and bss ok\n");
}

static void expect_reject(uint8_t *img, const char *name)
{
    uint64_t before;
    struct as *as = new_as(&before);
    struct loader_image out;
    int rc = loader_load_elf(as, img, IMAGE_SIZE, &out);

    fprintf(stderr, "loader: %s rc=%d\n", name, rc);
    assert(rc == MEM_EINVAL);
    assert(frame_free_count() == before);
    as_unref(as);
    assert(frame_free_count() == before + 1);
    printf("loader: reject %s ok\n", name);
}

static void test_rejections(uint8_t *img)
{
    struct elf64_ehdr *eh = (struct elf64_ehdr *)img;
    struct elf64_phdr *ph = (struct elf64_phdr *)(img + sizeof(*eh));
    uint16_t saved16;
    uint64_t saved64;

    saved16 = eh->e_machine;
    eh->e_machine = 0;
    expect_reject(img, "bad machine");
    eh->e_machine = saved16;

    saved16 = eh->e_type;
    eh->e_type = 3;
    expect_reject(img, "bad type");
    eh->e_type = saved16;

    saved16 = eh->e_phentsize;
    eh->e_phentsize = 1;
    expect_reject(img, "bad phdr size");
    eh->e_phentsize = saved16;

    saved64 = eh->e_phoff;
    eh->e_phoff = 1; /* 未对齐的 phdr 会成为 S-mode unaligned load */
    expect_reject(img, "unaligned phdr");
    eh->e_phoff = saved64;

    saved64 = ph[0].p_filesz;
    ph[0].p_filesz = ph[0].p_memsz + 1;
    expect_reject(img, "filesz exceeds memsz");
    ph[0].p_filesz = saved64;

    ph[0].p_flags = PF_R | PF_W | PF_X;
    expect_reject(img, "W+X");
    ph[0].p_flags = PF_R | PF_X;

    saved64 = eh->e_entry;
    eh->e_entry = USER_VA + 0x300;
    expect_reject(img, "non-executable entry");
    eh->e_entry = saved64;
}

static size_t add_member(uint8_t *archive, size_t off, const char *name,
                         uint8_t seed, size_t len)
{
    size_t name_len = strlen(name);

    put32(archive + off, (uint32_t)name_len);
    put32(archive + off + 4, (uint32_t)len);
    off += 8;
    memcpy(archive + off, name, name_len);
    off += name_len;
    for (size_t i = 0; i < len; i++) {
        archive[off + i] = (uint8_t)(seed + i);
    }
    off += len;
    while ((off & 3) != 0) {
        archive[off++] = 0;
    }
    return off;
}

static void test_initrd(void)
{
    static const char *names[] = {"init", "sm", "pm", "fs", "console"};
    static const size_t lens[] = {31, 3, 17, 0, 65};
    uint8_t archive[1024] = {0};
    size_t off = 12;

    memcpy(archive, "MIRD", 4);
    put32(archive + 4, 1);
    put32(archive + 8, 5);
    for (unsigned i = 0; i < 5; i++) {
        off = add_member(archive, off, names[i], (uint8_t)(i * 11), lens[i]);
    }
    for (unsigned i = 0; i < 5; i++) {
        const void *data = NULL;
        size_t size = 0;

        assert(loader_find_initrd(archive, off, names[i], &data, &size) == 0);
        assert(size == lens[i]);
        for (size_t j = 0; j < size; j++) {
            assert(((const uint8_t *)data)[j] == (uint8_t)(i * 11 + j));
        }
    }
    assert(loader_find_initrd(archive, off, "missing", &(const void *){0},
                              &(size_t){0}) != 0);
    put32(archive + 4, 2);
    assert(loader_find_initrd(archive, off, "init", &(const void *){0},
                              &(size_t){0}) != 0);
    put32(archive + 4, 1);
    assert(loader_find_initrd(archive, off - 1, "console", &(const void *){0},
                              &(size_t){0}) != 0);
    printf("loader: MIRD five-member/version/truncation ok\n");
}

static void test_segment_rejects_non_user_backing(uint8_t *img)
{
    uint64_t before;
    struct as *as = as_create();
    struct loader_image out;
    pfn_t pfn;

    assert(as != NULL);
    before = frame_free_count();
    pfn = frame_alloc();
    assert(pfn != PFN_NONE);
    /* 模拟设备 MMIO：段所在页已映射，但没有 U 位。 */
    assert(as_map(as, USER_VA, pfn, MEM_R | MEM_KERNEL) == MEM_OK);
    frame_unref(pfn);
    assert(loader_load_elf(as, img, IMAGE_SIZE, &out) == MEM_EPERM);
    /* 失败回滚不能把这块无 U 映射一起 unmap。 */
    assert(as_translate(as, USER_VA + 0x200, NULL, NULL) == MEM_OK);
    as_unref(as);
    /* data page + AS root 都回到池里。 */
    assert(frame_free_count() == before + 1);
    printf("loader: non-user backing at segment VA rejected without rollback damage\n");
}

static void test_map_failure_rollback(uint8_t *img)
{
    void *memory = NULL;
    uint64_t before;
    struct as *as;
    struct loader_image out;
    struct elf64_ehdr *eh = (struct elf64_ehdr *)img;
    struct elf64_phdr *ph = (struct elf64_phdr *)(img + sizeof(*eh));

    assert(posix_memalign(&memory, PAGE_SIZE, 4 * PAGE_SIZE) == 0);
    assert(frame_init(TEST_BASE, 4 * PAGE_SIZE, memory) == MEM_OK);
    ph[1].p_vaddr = USER_VA + PAGE_SIZE + 0x220;
    as = as_create();
    assert(as != NULL);
    before = frame_free_count();
    assert(loader_load_elf(as, img, IMAGE_SIZE, &out) == MEM_ENOMEM);
    assert(as_translate(as, USER_VA + 0x200, NULL, NULL) == MEM_EFAULT);
    assert(as_translate(as, USER_VA + PAGE_SIZE + 0x220, NULL, NULL) == MEM_EFAULT);
    as_unref(as);
    assert(frame_free_count() == before + 1);
    free(memory);
    printf("loader: second-page allocation failure rolls back mappings and frames\n");
}

int main(void)
{
    void *memory = NULL;
    uint8_t *img;

    assert(posix_memalign(&memory, PAGE_SIZE, TEST_SIZE) == 0);
    img = malloc(IMAGE_SIZE);
    assert(img != NULL);
    object_system_init();
    assert(frame_init(TEST_BASE, TEST_SIZE, memory) == MEM_OK);
    make_elf(img);
    test_initrd();
    test_shared_partial_page_and_bss(img);
    test_rejections(img);
    test_segment_rejects_non_user_backing(img);
    test_map_failure_rollback(img);
    printf("loader: all tests passed\n");
    free(img);
    free(memory);
    return 0;
}