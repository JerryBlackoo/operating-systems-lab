#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>

#define PORT 40000
#define BUF_SIZE 1024

int main() {
    int sockfd;
    struct sockaddr_in server_addr, client_addr;
    char buffer[BUF_SIZE];
    socklen_t addr_len;

    // TODO: 创建 UDP socket
    sockfd = /* 填写socket调用 */;
    if (sockfd < 0) {
        perror("socket creation failed");
        exit(EXIT_FAILURE);
    }

    memset(&server_addr, 0, sizeof(server_addr));
    server_addr.sin_family = AF_INET;
    server_addr.sin_addr.s_addr = INADDR_ANY;
    server_addr.sin_port = htons(PORT);

    // TODO: 绑定端口
    if (/* 填写bind调用 */ < 0) {
        perror("bind failed");
        close(sockfd);
        exit(EXIT_FAILURE);
    }

    printf("[SERVER port=%d] UDP Server listening on port %d...\n", PORT, PORT);
    fflush(stdout);

    while (1) {
        memset(buffer, 0, BUF_SIZE);
        addr_len = sizeof(client_addr);   // reset before every recvfrom

        // TODO: 接收数据
        int n = /* 填写recvfrom调用，注意最后一个参数是 &addr_len */;
        if (n < 0) {
            perror("recvfrom failed");
            continue;
        }

        buffer[n] = '\0';
        printf("[SERVER port=%d] Received from %s:%d: %s\n",
               PORT,
               inet_ntoa(client_addr.sin_addr),
               ntohs(client_addr.sin_port),
               buffer);
        fflush(stdout);

        // TODO: 发送回显（提示：用 sizeof(client_addr) 做地址长度）
        /* 填写sendto调用 */;
    }

    close(sockfd);
    return 0;
}