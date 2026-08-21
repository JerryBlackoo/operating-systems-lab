#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "disk.h"
#include "fs.h"

#define CMD_SIZE 256
#define ARG_SIZE 128

void print_help() {
    printf("Available commands:\n");
    printf("  format              - Format the disk\n");
    printf("  mount               - Mount the filesystem\n");
    printf("  create <filename>   - Create a file\n");
    printf("  open <filename>     - Open a file\n");
    printf("  close <fd>          - Close a file descriptor\n");
    printf("  read <fd> <size>    - Read from file descriptor\n");
    printf("  write <fd> <data>   - Write to file descriptor\n");
    printf("  delete <filename>   - Delete a file\n");
    printf("  ls                  - List files\n");
    printf("  help                - Show this help\n");
    printf("  exit                - Exit\n");
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        printf("Usage: %s <disk_image> [command]\n", argv[0]);
        return 1;
    }

    const char *disk_name = argv[1];
    Disk *disk = disk_init(disk_name);
    if (!disk) {
        printf("Failed to open disk: %s\n", disk_name);
        return 1;
    }

    FileSystem *fs = fs_init(disk);

    // 命令行模式
    if (argc > 2) {
        if (strcmp(argv[2], "format") == 0) {
            fs_format(disk);
        } else if (strcmp(argv[2], "mount") == 0) {
            fs_mount(fs);
        } else if (strcmp(argv[2], "ls") == 0) {
            fs_ls(fs);
        }
        disk_close(disk);
        return 0;
    }

    // 交互模式
    print_help();
    char line[CMD_SIZE];

    while (1) {
        printf("\nfs> ");
        if (!fgets(line, CMD_SIZE, stdin)) break;

        char cmd[ARG_SIZE], arg1[ARG_SIZE], arg2[ARG_SIZE];
        int args = sscanf(line, "%s %s %s", cmd, arg1, arg2);
        if (args <= 0) continue;

        if (strcmp(cmd, "exit") == 0) {
            break;
        } else if (strcmp(cmd, "help") == 0) {
            print_help();
        } else if (strcmp(cmd, "format") == 0) {
            fs_format(disk);
        } else if (strcmp(cmd, "mount") == 0) {
            fs_mount(fs);
        } else if (strcmp(cmd, "create") == 0) {
            // TODO: 解析参数并调用 fs_create
            if (args < 2) printf("Usage: create <filename>\n");
        } else if (strcmp(cmd, "open") == 0) {
            // TODO: 解析参数并调用 fs_open
            if (args < 2) printf("Usage: open <filename>\n");
        } else if (strcmp(cmd, "close") == 0) {
            // TODO: 解析参数并调用 fs_close
            if (args < 2) printf("Usage: close <fd>\n");
        } else if (strcmp(cmd, "read") == 0) {
            // TODO: 解析参数并调用 fs_read
            if (args < 3) printf("Usage: read <fd> <size>\n");
        } else if (strcmp(cmd, "write") == 0) {
            // TODO: 解析参数并调用 fs_write
            if (args < 3) printf("Usage: write <fd> <data>\n");
        } else if (strcmp(cmd, "delete") == 0) {
            // TODO: 解析参数并调用 fs_delete
            if (args < 2) printf("Usage: delete <filename>\n");
        } else if (strcmp(cmd, "ls") == 0) {
            fs_ls(fs);
        } else {
            printf("Unknown command: %s\n", cmd);
        }
    }

    fs_sync(fs);
    disk_close(disk);
    return 0;
}
