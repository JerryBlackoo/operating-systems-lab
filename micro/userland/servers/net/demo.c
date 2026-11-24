#include "udp.h"
#include "demo.h"

enum { IPV4_UDP_HEADERS = 28, EXACT_PAYLOAD = 128 - IPV4_UDP_HEADERS };

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
    CHECK(net_bind(&receiver, 0, 8) == NET_BAD_ARG);
    CHECK(net_bind(&receiver, 2, 0) == NET_BAD_ARG);
    CHECK(net_send(&sender, 2, 9010, dest, "x", 1) == NET_DENIED);
    CHECK(net_send(&sender, 1, 9010, (struct net_address){0xe0000001, 7}, "x", 1) == NET_BAD_ARG);
    CHECK(net_send(&sender, 1, 9010, (struct net_address){receiver.local_ip, 0}, "x", 1) == NET_BAD_ARG);
    CHECK(net_send(&sender, 1, 9010, dest, NULL, 1) == NET_BAD_ARG);
    uint8_t large[101] = {0};
    CHECK(net_send(&sender, 1, 9010, dest, large, sizeof(large)) == NET_TOO_LARGE);

    uint8_t exact[EXACT_PAYLOAD];
    memset(exact, 0, sizeof(exact));
    CHECK(net_send(&sender, 1, 9010, dest, exact, sizeof(exact)) == NET_OK);
    CHECK(link.len == sender.mtu && link.packet[0] == 0x45);
    CHECK(link.packet[2] == 0 && link.packet[3] == (uint8_t)sender.mtu);
    CHECK(link.packet[6] == 0x40 && link.packet[7] == 0 && link.packet[8] == 64);
    CHECK(link.packet[9] == 17 && link.packet[12] == 0x0a && link.packet[15] == 1);
    CHECK(link.packet[16] == 0x0a && link.packet[19] == 2);
    CHECK(link.packet[20] == 0x23 && link.packet[21] == 0x32);
    CHECK(link.packet[22] == 0 && link.packet[23] == 7);

    CHECK(net_send(&sender, 1, 9010, dest, "odd", 3) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len - 1) == NET_BAD_PACKET);
    link.packet[link.len - 1] ^= 1;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    link.packet[link.len - 1] ^= 1;
    link.packet[8] ^= 1;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    link.packet[8] ^= 1;
    link.packet[24] -= 1;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    link.packet[24] += 1;
    link.packet[6] |= 0x20;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_BAD_PACKET);
    link.packet[6] &= 0xdf;

    uint8_t udp_sum[2] = {link.packet[26], link.packet[27]};
    link.packet[26] = link.packet[27] = 0;
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    link.packet[26] = udp_sum[0];
    link.packet[27] = udp_sum[1];
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && out.len == 3 && !memcmp(out.data, "odd", out.len));

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

    CHECK(net_send(&sender, 1, 9010, dest, "aaa", 3) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    CHECK(net_send(&sender, 1, 9010, dest, "bbb", 3) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && !memcmp(out.data, "aaa", 3));
    CHECK(net_send(&sender, 1, 9010, dest, "ccc", 3) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_OK);
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && !memcmp(out.data, "bbb", 3));
    CHECK(net_recv(&receiver, 2, 7, &out) == NET_OK && !memcmp(out.data, "ccc", 3));

    link.busy = true;
    CHECK(net_send(&sender, 1, 9010, dest, "x", 1) == NET_LINK_ERROR);
    link.busy = false;
    CHECK(net_unbind(&receiver, 3, 7) == NET_DENIED);
    CHECK(net_unbind(&receiver, 2, 7) == NET_OK && net_recv(&receiver, 2, 7, &out) == NET_NOT_FOUND);
    CHECK(net_send(&sender, 1, 9010, dest, "x", 1) == NET_OK);
    CHECK(net_rx(&receiver, link.packet, link.len) == NET_NOT_FOUND);
    CHECK(net_bind(&receiver, 3, 7) == NET_OK && net_unbind(&receiver, 3, 7) == NET_OK);

    struct net capacity = {.local_ip = 0x0a000009, .mtu = 64};
    for (uint16_t local = 1; local <= NET_PORTS; local++)
        CHECK(net_bind(&capacity, 9, local) == NET_OK);
    CHECK(net_bind(&capacity, 9, NET_PORTS + 1) == NET_NO_SPACE);
    CHECK(net_unbind(&capacity, 9, 2) == NET_OK && net_bind(&capacity, 9, 9) == NET_OK);

    demo_log("UDP: IPv4/UDP checksums, datagram FIFO, ownership and packet rejection");
}

DEMO_MODULE(net, true, run, NULL);
