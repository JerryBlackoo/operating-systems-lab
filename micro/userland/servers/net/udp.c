#include "udp.h"
#include "demo.h"

enum { IP_HEADER = 20, UDP_HEADER = 8, HEADERS = IP_HEADER + UDP_HEADER, IP_UDP = 17 };

static uint16_t be16(const uint8_t *p)
{
    return (uint16_t)p[0] << 8 | p[1];
}

static uint32_t be32(const uint8_t *p)
{
    return (uint32_t)be16(p) << 16 | be16(p + 2);
}

static void put16(uint8_t *p, uint16_t value)
{
    p[0] = value >> 8;
    p[1] = value;
}

static void put32(uint8_t *p, uint32_t value)
{
    put16(p, value >> 16);
    put16(p + 2, value);
}

static uint32_t sum_bytes(const uint8_t *p, size_t len)
{
    uint32_t sum = 0;
    for (; len >= 2; len -= 2, p += 2)
        sum += be16(p);
    if (len)
        sum += (uint16_t)p[0] << 8;
    return sum;
}

static uint16_t checksum(uint32_t sum)
{
    while (sum >> 16)
        sum = (sum & 0xffff) + (sum >> 16);
    return (uint16_t)~sum;
}

static uint16_t udp_checksum(const uint8_t *p, size_t udp_len)
{
    uint32_t pseudo_header = sum_bytes(p + 12, 8) + IP_UDP + udp_len;
    return checksum(pseudo_header + sum_bytes(p + IP_HEADER, udp_len));
}

static bool unicast(uint32_t ip)
{
    uint8_t first = ip >> 24;
    return first != 0 && first < 224;
}

static struct net_port *find_port(struct net *net, uint16_t local)
{
    for (unsigned i = 0; i < NET_PORTS; i++) {
        if (net->ports[i].owner && net->ports[i].local == local)
            return &net->ports[i];
    }
    return NULL;
}

enum net_result net_bind(struct net *net, uint32_t caller, uint16_t local)
{
    if (!caller || !local)
        return NET_BAD_ARG;
    if (find_port(net, local))
        return NET_EXISTS;
    for (unsigned i = 0; i < NET_PORTS; i++) {
        if (!net->ports[i].owner) {
            net->ports[i] = (struct net_port){.owner = caller, .local = local};
            return NET_OK;
        }
    }
    return NET_NO_SPACE;
}

enum net_result net_unbind(struct net *net, uint32_t caller, uint16_t local)
{
    struct net_port *port = find_port(net, local);
    if (!port)
        return NET_NOT_FOUND;
    if (port->owner != caller)
        return NET_DENIED;
    *port = (struct net_port){0};
    return NET_OK;
}

enum net_result net_send(struct net *net, uint32_t caller, uint16_t local, struct net_address dest,
                         const void *bytes, size_t len)
{
    struct net_port *port = find_port(net, local);
    if (!port)
        return NET_NOT_FOUND;
    if (port->owner != caller)
        return NET_DENIED;
    if (!net->send_packet || !unicast(net->local_ip) || !unicast(dest.ip) || !dest.port ||
        (len && !bytes))
        return NET_BAD_ARG;
    if (len > NET_PAYLOAD || HEADERS + len > net->mtu)
        return NET_TOO_LARGE;

    uint8_t packet[NET_PACKET] = {0x45};
    const size_t total = HEADERS + len;
    put16(packet + 2, total);
    put16(packet + 6, 0x4000);
    packet[8] = 64;
    packet[9] = IP_UDP;
    put32(packet + 12, net->local_ip);
    put32(packet + 16, dest.ip);
    put16(packet + 10, checksum(sum_bytes(packet, IP_HEADER)));
    uint8_t *udp = packet + IP_HEADER;
    put16(udp, local);
    put16(udp + 2, dest.port);
    put16(udp + 4, UDP_HEADER + len);
    if (len)
        memcpy(packet + HEADERS, bytes, len);
    uint16_t sum = udp_checksum(packet, UDP_HEADER + len);
    put16(udp + 6, sum ? sum : 0xffff);

    return net->send_packet(net->link, packet, total) ? NET_OK : NET_LINK_ERROR;
}

enum net_result net_rx(struct net *net, const uint8_t *packet, size_t len)
{
    if (!packet || len < HEADERS || len > net->mtu || len > NET_PACKET)
        return NET_BAD_PACKET;
    if (packet[0] != 0x45 || be16(packet + 2) != len || (be16(packet + 6) & ~0x4000u) ||
        !packet[8] || packet[9] != IP_UDP || checksum(sum_bytes(packet, IP_HEADER)) ||
        be32(packet + 16) != net->local_ip || !unicast(net->local_ip) ||
        !unicast(be32(packet + 12)))
        return NET_BAD_PACKET;
    const uint8_t *udp = packet + IP_HEADER;
    if (be16(udp + 4) != len - IP_HEADER ||
        (be16(udp + 6) && udp_checksum(packet, len - IP_HEADER)))
        return NET_BAD_PACKET;

    struct net_port *port = find_port(net, be16(udp + 2));
    if (!port)
        return NET_NOT_FOUND;
    if (port->count == NET_QUEUE)
        return NET_NO_SPACE;
    unsigned tail = (port->head + port->count) % NET_QUEUE;
    struct net_datagram *msg = &port->pending[tail];
    msg->source = (struct net_address){.ip = be32(packet + 12), .port = be16(udp)};
    msg->len = len - HEADERS;
    memcpy(msg->data, packet + HEADERS, msg->len);
    port->count++;
    return NET_OK;
}

enum net_result net_recv(struct net *net, uint32_t caller, uint16_t local, struct net_datagram *out)
{
    struct net_port *port = find_port(net, local);
    if (!port)
        return NET_NOT_FOUND;
    if (port->owner != caller)
        return NET_DENIED;
    if (!out)
        return NET_BAD_ARG;
    if (!port->count)
        return NET_EMPTY;
    *out = port->pending[port->head];
    port->pending[port->head] = (struct net_datagram){0};
    port->head = (port->head + 1) % NET_QUEUE;
    port->count--;
    return NET_OK;
}
