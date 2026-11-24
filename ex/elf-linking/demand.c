#include <dlfcn.h>
#include <stdio.h>
#include <unistd.h>
int main(void) {
    write(1, "[main] started\n", 15);
    write(1, "[main] before dlopen\n", 21);
    void *h = dlopen("libcalc.so", RTLD_NOW | RTLD_LOCAL);
    if (!h) { fprintf(stderr, "dlopen failed: %s\n", dlerror()); return 1; }
    dlerror(); /* 清除此前的错误，再判断这次 dlsym 的结果。 */
    int (*add)(int, int) = (int (*)(int, int))dlsym(h, "add");
    const char *error = dlerror();
    if (error) { fprintf(stderr, "dlsym failed: %s\n", error); dlclose(h); return 1; }
    printf("[main] add(2,3) = %d\n", add(2, 3));
    dlclose(h);
    return 0;
}
