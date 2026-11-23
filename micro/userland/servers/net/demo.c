#include "udp.h"
#include "demo.h"

static struct link_fixture { uint8_t packet[NET_PACKET]; size_t len; bool busy; } link;

static bool capture(void *ctx, const uint8_t *packet, size_t len)
{
    struct link_fixture *wire = ctx;
    if (wire->busy)
        return false;
    CHECK(len <= sizeof(wire->packet));
    memcpy(wire->packet, packet, len);
    wire->len = len;
    return true;
}

static void run(void)
{
    static struct net sender = {
        .local_ip = 0x0a000001, .mtu = 128, .link = &link, .send_packet = capture,
    };
    static struct net receiver = {.local_ip = 0x0a000002, .mtu = 128};
    struct net_address dest = {.ip = receiver.local_ip, .port = 7};
    struct net_datagram out;

    CHECK(net_bind(&sender, 1, 9010) == NET_OK);
    CHECK(net_bind(&receiver, 2, 7) == NET_OK);
    CHECK(net_bind(&receiver, 3, 7) == NET_EXISTS);
    CHECK(net_send(&sender, 2, 9010, dest, "x", 1) == NET_DENIED);
    uint8_t large[101] = {0};
    CHECK(net_send(&sender, 1, 9010, dest, large, sizeof(large)) == NET_TOO_LARGE);

    CHECK(net_send(&sender, 1, 9010, dest, "odd", 3) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len - 1) == NET_BAD_PACKET);
    link.packet[link.len - 1] ^= 1;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    link.packet[link.len - 1] ^= 1;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    CHECK(net_send(&sender, 1, 9010, dest, "even", 4) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_NO_SPACE);
    CHECK(net_send(&sender, 1, 9010, (struct net_address){0x0a000003, 7}, "x", 1) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    CHECK(net_recv(&receiver, 3, 7, &out) == NET_DENIED);
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && out.len == 3 && !memcmp(out.data, "odd", out.len));
    CHECK(out.source.ip == sender.local_ip && out.source.port == 9010);
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && out.len == 4 && !memcmp(out.data, "even", out.len));
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_EMPTY);
    link.busy = true;
    CHECK(net_send(&sender, 1, 9010, dest, "x", 1) == NET_LINK_ERROR);
    CHECK(net_unbind(&receiver, 3, 7) == NET_DENIED);
    CHECK(net_unbind(&receiver, 2, 7) == NET_OK && net_recv(&receiver, 2, 7, &out) == NET_NOT_FOUND);
    demo_log("UDP: IPv4/UDP checksums, datagram FIFO, ownership and packet rejection");
}

DEMO_MODULE(net, true, run, NULL);
