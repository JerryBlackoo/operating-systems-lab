_Noreturn void kmain(void)
{
    const char *message = "kmain: 32-bit protected mode\n";
    for (const char *p = message; *p; ++p)
        __asm__ volatile ("outb %0, $0xe9" : : "a"((unsigned char)*p));

    for (;;)
        __asm__ volatile ("hlt");
}
