/* Minimal valid ELF executable used as the target of lmbench `lat_proc`
 * exec/shell micro-benchmarks.
 *
 * The oscomp test image ships /musl/hello and /glibc/hello as broken wrapper
 * text files (no shebang, pointing at a build path that does not exist in the
 * image), so execve() on them returns ENOEXEC and lat_proc measures the
 * failure path / busybox-ash spams "Exec format error". A real Linux + real
 * lmbench uses a genuine ELF here, so we ship one and overwrite the broken
 * wrappers at boot to measure the true fork+exec cost.
 *
 * Static minimal program: returns immediately, matching lmbench's own hello.c.
 */
int main(void) { return 0; }
