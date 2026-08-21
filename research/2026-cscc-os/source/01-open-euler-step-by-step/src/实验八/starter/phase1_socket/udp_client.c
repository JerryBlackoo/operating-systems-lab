#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>

#define PORT 40000
#define BUF_SIZE 1024

int main() {
    int sockfd;
    struct sockaddr_in server_addr;
    char buffer[BUF_SIZE];

    // TODO: 创建 UDP socket
    sockfd = /* 填写socket调用 */;
    if (sockfd < 0) {
        perror("socket creation failed");
        exit(EXIT_FAILURE);
    }

    memset(&server_addr, 0, sizeof(server_addr));
    server_addr.sin_family = AF_INET;
    server_addr.sin_port = htons(PORT);
    server_addr.sin_addr.s_addr = inet_addr("127.0.0.1");

    printf("UDP Client started. Type messages to send (type 'quit' to exit):\n");

    while (1) {
        printf("> ");
        if (fgets(buffer, BUF_SIZE, stdin) == NULL) break;

        // 去除换行符
        buffer[strcspn(buffer, "\n")] = '\0';

        if (strcmp(buffer, "quit") == 0) break;

        // TODO: 发送数据
        /* 填写sendto调用 */;

        // TODO: 接收回显
        memset(buffer, 0, BUF_SIZE);
        int n = /* 填写recvfrom调用 */;
        if (n > 0) {
            buffer[n] = '\0';
            printf("Server echo: %s\n", buffer);
        }
    }

    close(sockfd);
    printf("Client exited.\n");
    return 0;
}