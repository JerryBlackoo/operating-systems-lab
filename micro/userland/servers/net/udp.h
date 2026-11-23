#ifndef MICRO_MODEL_UDP_H
#define MICRO_MODEL_UDP_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { NET_PORTS = 4, NET_QUEUE = 2, NET_PAYLOAD = 256, NET_PACKET = 28 + NET_PAYLOAD };
enum net_result {
    NET_OK, NET_BAD_ARG, NET_EXISTS, NET_NO_SPACE, NET_NOT_FOUND, NET_DENIED,
    NET_TOO_LARGE, NET_EMPTY, NET_BAD_PACKET, NET_LINK_ERROR,
};

struct net_address { uint32_t ip; uint16_t port; };
struct net_datagram {
    struct net_address source;
    size_t len;
    uint8_t data[NET_PAYLOAD];
};
struct net_port {
    uint32_t owner;
    uint16_t local;
    unsigned head, count;
    struct net_datagram pending[NET_QUEUE];
};
struct net {
    uint32_t local_ip;
    size_t mtu;
    void *link;
    bool (*send_packet)(void *, const uint8_t *, size_t);
    struct net_port ports[NET_PORTS];
};

enum net_result net_bind(struct net *, uint32_t caller, uint16_t local);
enum net_result net_unbind(struct net *, uint32_t caller, uint16_t local);
enum net_result net_send(struct net *, uint32_t caller, uint16_t local, struct net_address dest,
                         const void *bytes, size_t len);
enum net_result net_rx(struct net *, const uint8_t *packet, size_t len);
enum net_result net_recv(struct net *, uint32_t caller, uint16_t local, struct net_datagram *);

#endif
