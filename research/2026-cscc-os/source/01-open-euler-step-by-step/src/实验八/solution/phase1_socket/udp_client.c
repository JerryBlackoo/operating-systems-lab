#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <arpa/inet.h>

#define DEFAULT_IP   "127.0.0.1"
#define DEFAULT_PORT 40000
#define BUF_SIZE 1024

int main(int argc, char *argv[]) {
    int sockfd, port = DEFAULT_PORT;
    const char *ip = DEFAULT_IP;
    struct sockaddr_in server_addr;
    char buffer[BUF_SIZE];
    socklen_t addr_len = sizeof(server_addr);

    if (argc >= 2) ip   = argv[1];
    if (argc >= 3) port = atoi(argv[2]);

    sockfd = socket(AF_INET, SOCK_DGRAM, 0);
    if (sockfd < 0) { perror("socket creation failed"); exit(EXIT_FAILURE); }

    memset(&server_addr, 0, sizeof(server_addr));
    server_addr.sin_family = AF_INET;
    server_addr.sin_port = htons(port);
    if (inet_pton(AF_INET, ip, &server_addr.sin_addr) <= 0) {
        perror("invalid address");
        close(sockfd);
        exit(EXIT_FAILURE);
    }

    struct timeval tv = { .tv_sec = 3, .tv_usec = 0 };
    if (setsockopt(sockfd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv)) < 0) {
        perror("setsockopt SO_RCVTIMEO failed (continuing anyway)");
    }

    if (argc >= 4) {
        const char *msg = argv[3];
        printf("Sending to %s:%d: %s\n", ip, port, msg);
        sendto(sockfd, msg, strlen(msg), 0,
               (struct sockaddr *)&server_addr, addr_len);
        memset(buffer, 0, BUF_SIZE);
        ssize_t n = recvfrom(sockfd, buffer, BUF_SIZE-1, 0, NULL, NULL);
        if (n < 0) {
            printf("No reply (timeout / filtered)\n");
            close(sockfd);
            return 1;
        }
        printf("Echo reply: %s\n", buffer);
    } else {
        printf("UDP Client %s:%d (type messages, 'quit' to exit):\n", ip, port);
        while (1) {
            printf("> ");
            if (fgets(buffer, BUF_SIZE, stdin) == NULL) break;
            buffer[strcspn(buffer, "\n")] = '\0';
            if (strcmp(buffer, "quit") == 0) break;
            sendto(sockfd, buffer, strlen(buffer), 0,
                   (struct sockaddr *)&server_addr, addr_len);
            memset(buffer, 0, BUF_SIZE);
            ssize_t n = recvfrom(sockfd, buffer, BUF_SIZE-1, 0, NULL, NULL);
            if (n < 0) printf("No reply (timeout / filtered)\n");
            else       printf("Reply: %s\n", buffer);
        }
    }
    close(sockfd);
    return 0;
}
