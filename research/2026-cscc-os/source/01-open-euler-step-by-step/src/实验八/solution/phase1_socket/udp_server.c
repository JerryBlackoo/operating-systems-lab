#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>

#define DEFAULT_PORT 40000
#define BUF_SIZE 1024

int main(int argc, char *argv[]) {
    int sockfd, port = DEFAULT_PORT;
    struct sockaddr_in server_addr, client_addr;
    char buffer[BUF_SIZE];
    socklen_t addr_len;

    if (argc >= 2) port = atoi(argv[1]);

    sockfd = socket(AF_INET, SOCK_DGRAM, 0);
    if (sockfd < 0) { perror("socket creation failed"); exit(EXIT_FAILURE); }

    memset(&server_addr, 0, sizeof(server_addr));
    server_addr.sin_family = AF_INET;
    server_addr.sin_addr.s_addr = INADDR_ANY;
    server_addr.sin_port = htons(port);

    if (bind(sockfd, (struct sockaddr *)&server_addr, sizeof(server_addr)) < 0) {
        perror("bind failed");
        close(sockfd);
        exit(EXIT_FAILURE);
    }

    printf("[SERVER port=%d] UDP Server listening on port %d...\n", port, port);
    fflush(stdout);

    while (1) {
        memset(buffer, 0, BUF_SIZE);
        addr_len = sizeof(client_addr);   // reset before every recvfrom
        ssize_t n = recvfrom(sockfd, buffer, BUF_SIZE-1, 0,
                             (struct sockaddr *)&client_addr, &addr_len);
        if (n < 0) continue;
        buffer[n] = '\0';
        printf("[SERVER port=%d] Received (%s:%d): %s\n",
               port,
               inet_ntoa(client_addr.sin_addr), ntohs(client_addr.sin_port), buffer);
        fflush(stdout);
        sendto(sockfd, buffer, n, 0,
               (struct sockaddr *)&client_addr, sizeof(client_addr));
    }
    close(sockfd);
    return 0;
}
