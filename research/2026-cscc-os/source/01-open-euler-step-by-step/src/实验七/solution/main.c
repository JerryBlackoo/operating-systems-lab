#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "disk.h"
#include "fs.h"

#define CMD_SIZE 2048
#define ARG_SIZE 128

static void print_help(void) {
    printf("Available commands:\n");
    printf("  format                   - Format the disk\n");
    printf("  mount                    - Mount the filesystem\n");
    printf("  create <filename>        - Create a new file and auto-open (return fd)\n");
    printf("  open   <filename>        - Open an existing file by name\n");
    printf("  close  <fd>              - Close a file descriptor\n");
    printf("  read   <fd|name> [size]  - Read from fd, or read entire file by name\n");
    printf("  write  <fd|name> <data>  - Write data to fd or filename\n");
    printf("                           - (if no data, read payload from stdin)\n");
    printf("  delete <filename>        - Delete a file by name\n");
    printf("  ls                       - List files in root directory\n");
    printf("  help                     - Show this help\n");
    printf("  exit                     - Sync and exit\n");
}

// Auto-mount if disk has been formatted; return 0 on success, -1 if needs format
static int try_auto_mount(FileSystem *fs) {
    uint8_t sb_buf[BLOCK_SIZE];
    if (disk_read(fs->disk, SUPERBLOCK_BLOCK, sb_buf) != 0) return -1;
    Superblock sb;
    memcpy(&sb, sb_buf, sizeof(sb));
    if (sb.magic != 0xDEADBEEF) return -1;
    fs->sb = sb;
    fs->mounted = 1;
    memset(fs->open_files, 0, sizeof(fs->open_files));
    return 0;
}

// Helper: resolve to fd given an fd-string or a filename
// Returns fd (>=0) on success, -1 on error
static int resolve_fd(FileSystem *fs, const char *arg) {
    if (!arg || !*arg) return -1;
    // If purely numeric, treat as fd
    char *endp = NULL;
    long v = strtol(arg, &endp, 10);
    if (endp && *endp == '\0' && v >= 0 && v < MAX_OPEN_FILES) {
        if (fs->open_files[v].used) return (int)v;
        return -1;
    }
    // Otherwise treat as filename -> open it
    return fs_open(fs, arg, 0);
}

// Read all of stdin into a malloc'd buffer (up to 4MB)
static char *read_all_stdin(size_t *out_len) {
    size_t cap = 4096, len = 0;
    char *buf = malloc(cap);
    if (!buf) return NULL;
    for (;;) {
        if (len + 2048 > cap) {
            if (cap >= 4 * 1024 * 1024) break;
            cap *= 2;
            char *nb = realloc(buf, cap);
            if (!nb) { free(buf); return NULL; }
            buf = nb;
        }
        size_t r = fread(buf + len, 1, cap - len, stdin);
        if (r == 0) break;
        len += r;
    }
    *out_len = len;
    return buf;
}

// Run one command. Returns: 1 = exit requested, 0 = ok continue, -1 = command failed
static int run_one_cmd(FileSystem *fs, const char *cmd_line) {
    char line[CMD_SIZE];
    strncpy(line, cmd_line, sizeof(line) - 1);
    line[sizeof(line) - 1] = '\0';

    char cmd[ARG_SIZE];
    char arg1[ARG_SIZE];
    char arg2[ARG_SIZE];

    int args = sscanf(line, "%127s %127s %127s", cmd, arg1, arg2);
    if (args <= 0) return 0;

    const char *p = line;
    while (*p && *p != ' ') p++;
    if (*p == ' ') p++;
    while (*p && *p != ' ') p++;
    if (*p == ' ') p++;

    int cmd_rc = 0;  // 0 = success, -1 = failure

    if (strcmp(cmd, "exit") == 0) {
        return 1;
    } else if (strcmp(cmd, "help") == 0) {
        print_help();
    } else if (strcmp(cmd, "format") == 0) {
        if (fs_format(fs->disk) == 0) {
            try_auto_mount(fs);
            printf("Format successful, filesystem auto-mounted\n");
        } else {
            printf("Format failed\n");
            cmd_rc = -1;
        }
    } else if (strcmp(cmd, "mount") == 0) {
        if (fs->mounted) {
            printf("Already mounted\n");
        } else if (fs_mount(fs) == 0) {
            /* ok */
        } else {
            printf("Mount failed: invalid filesystem (need format first?)\n");
            cmd_rc = -1;
        }
    } else if (strcmp(cmd, "create") == 0) {
        if (args < 2) {
            printf("Usage: create <filename>\n");
            cmd_rc = -1;
        } else if (!fs->mounted) {
            printf("Not mounted, please format or mount first\n");
            cmd_rc = -1;
        } else {
            int fd = fs_create(fs, arg1, 1);
            if (fd >= 0) {
                printf("File created and opened, fd=%d (inode=%d)\n",
                       fd, fs->open_files[fd].inode_num);
            } else {
                printf("Failed to create file (exists? no space?)\n");
                cmd_rc = -1;
            }
        }
    } else if (strcmp(cmd, "open") == 0) {
        if (args < 2) {
            printf("Usage: open <filename>\n");
            cmd_rc = -1;
        } else if (!fs->mounted) {
            printf("Not mounted\n");
            cmd_rc = -1;
        } else {
            int fd = fs_open(fs, arg1, 0);
            if (fd >= 0) {
                printf("File opened, fd=%d\n", fd);
            } else {
                printf("Failed to open file\n");
                cmd_rc = -1;
            }
        }
    } else if (strcmp(cmd, "close") == 0) {
        if (args < 2) {
            printf("Usage: close <fd>\n");
            cmd_rc = -1;
        } else {
            int fd = atoi(arg1);
            if (fs_close(fs, fd) == 0) printf("File closed\n");
            else { printf("Failed to close file (invalid fd)\n"); cmd_rc = -1; }
        }
    } else if (strcmp(cmd, "read") == 0) {
        if (args < 2) {
            printf("Usage: read <fd|filename> [size]\n");
            cmd_rc = -1;
        } else if (!fs->mounted) {
            printf("Not mounted\n");
            cmd_rc = -1;
        } else {
            int size;
            if (args >= 3) size = atoi(arg2);
            else           size = 16384;
            int fd = resolve_fd(fs, arg1);
            if (fd < 0) { printf("Failed: cannot open '%s' (not found or invalid fd)\n", arg1); cmd_rc = -1; }
            else {
                char *buffer = (char *)malloc(size + 1);
                int n = fs_read(fs, fd, buffer, size);
                if (n >= 0) {
                    buffer[n] = '\0';
                    printf("Read %d bytes:\n%.*s\n", n, n, buffer);
                } else {
                    printf("Failed to read file\n");
                    cmd_rc = -1;
                }
                free(buffer);
                char *endp = NULL;
                long v = strtol(arg1, &endp, 10);
                int opened_by_name = !(endp && *endp == '\0' && v >= 0 && v < MAX_OPEN_FILES);
                if (opened_by_name) fs_close(fs, fd);
            }
        }
    } else if (strcmp(cmd, "write") == 0) {
        if (args < 2) {
            printf("Usage: write <fd|filename> [<data...>]\n");
            printf("  If <data> is omitted, payload is read from stdin until EOF.\n");
            cmd_rc = -1;
        } else if (!fs->mounted) {
            printf("Not mounted\n");
            cmd_rc = -1;
        } else {
            int fd = resolve_fd(fs, arg1);
            if (fd < 0) { printf("Failed: cannot open '%s'\n", arg1); cmd_rc = -1; }
            else {
                const char *inline_data = p;
                int wrote = -1;
                if (inline_data && *inline_data) {
                    wrote = fs_write(fs, fd, inline_data, strlen(inline_data));
                } else {
                    size_t slen = 0;
                    char *sbuf = read_all_stdin(&slen);
                    if (sbuf) {
                        wrote = fs_write(fs, fd, sbuf, slen);
                        free(sbuf);
                    }
                }
                if (wrote >= 0) printf("Wrote %d bytes\n", wrote);
                else            { printf("Failed to write file\n"); cmd_rc = -1; }

                FileDescriptor *ff = &fs->open_files[fd];
                Inode *ip = read_inode(fs, ff->inode_num);
                if (ip) {
                    if ((int)ip->size < ff->offset) {
                        ip->size = ff->offset;
                        write_inode(fs, ff->inode_num, ip);
                    }
                }
                char *endp = NULL;
                long v = strtol(arg1, &endp, 10);
                int opened_by_name = !(endp && *endp == '\0' && v >= 0 && v < MAX_OPEN_FILES);
                if (opened_by_name) fs_close(fs, fd);
            }
        }
    } else if (strcmp(cmd, "delete") == 0) {
        if (args < 2) {
            printf("Usage: delete <filename>\n");
            cmd_rc = -1;
        } else if (!fs->mounted) {
            printf("Not mounted\n");
            cmd_rc = -1;
        } else {
            if (fs_delete(fs, arg1) == 0) printf("File deleted\n");
            else { printf("Failed to delete file (open? not exist?)\n"); cmd_rc = -1; }
        }
    } else if (strcmp(cmd, "ls") == 0) {
        if (!fs->mounted) {
            printf("Not mounted, please format or mount first\n");
            cmd_rc = -1;
        } else {
            fs_ls(fs);
        }
    } else {
        printf("Unknown command: %s (type 'help')\n", cmd);
        cmd_rc = -1;
    }
    if (cmd_rc < 0) return -1;
    return 0;
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        printf("Usage: %s <disk_image> [command [args...]]\n", argv[0]);
        return 1;
    }

    const char *disk_name = argv[1];
    Disk *disk = disk_init(disk_name);
    if (!disk) {
        printf("Failed to open disk image: %s\n", disk_name);
        return 1;
    }

    FileSystem *fs = fs_init(disk);
    if (!fs) {
        printf("Failed to initialize filesystem\n");
        disk_close(disk);
        return 1;
    }

    // Best-effort auto-mount; user can run 'format' or 'mount' explicitly
    try_auto_mount(fs);

    if (argc > 2) {
        char buf[CMD_SIZE] = {0};
        size_t off = 0;
        for (int i = 2; i < argc && off < sizeof(buf) - 2; i++) {
            size_t l = strlen(argv[i]);
            if (off + l + 2 >= sizeof(buf)) break;
            if (i > 2) buf[off++] = ' ';
            memcpy(buf + off, argv[i], l);
            off += l;
        }
        buf[off] = '\0';
        int rc = run_one_cmd(fs, buf);
        fs_sync(fs);
        disk_close(disk);
        return (rc < 0) ? 1 : 0;
    }

    // Interactive mode
    printf("Simple Filesystem Simulator (disk: %s)\n", disk_name);
    if (fs->mounted) printf("Filesystem auto-mounted\n");
    else             printf("Disk not formatted yet — run 'format' first\n");
    print_help();

    char line[CMD_SIZE];
    for (;;) {
        printf("\nfs> ");
        if (!fgets(line, sizeof(line), stdin)) break;
        size_t l = strlen(line);
        if (l > 0 && line[l-1] == '\n') line[l-1] = '\0';
        int rc = run_one_cmd(fs, line);
        if (rc == 1) break;
    }

    fs_sync(fs);
    disk_close(disk);
    return 0;
}
