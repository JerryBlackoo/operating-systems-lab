enum thread_state {
    UNUSED,
    READY,
    RUNNING,
    EXITED,
};

/* 内存从低到高，与 switch.S 的 pop/pop/pop/pop/ret 对应。 */
struct switch_frame {
    unsigned int edi;
    unsigned int esi;
    unsigned int ebx;
    unsigned int ebp;
    void (*resume)(void);
};

struct thread {
    struct switch_frame *sp;
    void (*entry)(void);
    enum thread_state state;
    char name;
};

struct thread_stack {
    unsigned char space[4096 - sizeof(struct switch_frame)];
    struct switch_frame initial;
};

enum { THREAD_COUNT = 2, ROUNDS = 3 };

_Static_assert(sizeof(struct switch_frame) == 20, "i386 switch frame");
_Static_assert(sizeof(struct thread_stack) == 4096, "thread stack size");

static struct thread threads[THREAD_COUNT];
static _Alignas(16) struct thread_stack stacks[THREAD_COUNT];
static struct thread *current;
static struct switch_frame *scheduler_sp;
static unsigned int shared_counter;

extern void switch_context(struct switch_frame **old_sp, struct switch_frame *new_sp);
extern void thread_entry(void);

static void put_char(char ch)
{
    __asm__ volatile ("outb %0, $0xe9" : : "a"((unsigned char)ch));
}

static void print(const char *message)
{
    while (*message)
        put_char(*message++);
}

static _Noreturn void finish(unsigned int status)
{
    __asm__ volatile ("outl %0, $0xf4" : : "a"(status));
    for (;;)
        __asm__ volatile ("hlt");
}

static void thread_create(unsigned int slot, char name, void (*entry)(void))
{
    struct switch_frame *frame = &stacks[slot].initial;
    *frame = (struct switch_frame){ .resume = thread_entry };
    threads[slot] = (struct thread){
        .sp = frame,
        .entry = entry,
        .state = READY,
        .name = name,
    };
}

static void thread_yield(void)
{
    current->state = READY;
    switch_context(&current->sp, scheduler_sp);
}

_Noreturn void run_current_thread(void)
{
    current->entry();
    current->state = EXITED;
    switch_context(&current->sp, scheduler_sp);

    print("FAIL: exited thread was resumed\n");
    finish(0x11);
}

static void dispatch_threads(void)
{
    unsigned int dispatched;
    do {
        dispatched = 0;
        for (unsigned int i = 0; i < THREAD_COUNT; ++i) {
            if (threads[i].state != READY)
                continue;

            current = &threads[i];
            current->state = RUNNING;
            switch_context(&scheduler_sp, current->sp);
            current = 0;
            ++dispatched;
        }
    } while (dispatched);
}

static void worker(void)
{
    for (unsigned int local_count = 0; local_count < ROUNDS; ++local_count) {
        put_char(current->name);
        print(" local=");
        put_char('0' + local_count);
        print(" shared=");
        put_char('0' + ++shared_counter);
        put_char('\n');
        thread_yield();
    }
}

_Noreturn void kmain(void)
{
    print("kmain: 32-bit protected mode\n");
    thread_create(0, 'A', worker);
    thread_create(1, 'B', worker);
    dispatch_threads();

    if (threads[0].state != EXITED || threads[1].state != EXITED ||
        shared_counter != THREAD_COUNT * ROUNDS) {
        print("FAIL: incomplete threads\n");
        finish(0x11);
    }

    print("PASS: both threads exited\n");
    finish(0x10);
}
