#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <bpf/bpf_endian.h>

#define ETH_P_IP 0x0800
#define IPPROTO_UDP 17
#define ALLOW_PORT 40000

struct ethhdr {
    unsigned char h_dest[6];
    unsigned char h_source[6];
    unsigned short h_proto;
};

struct iphdr {
    unsigned char ihl:4, version:4;
    unsigned char tos;
    unsigned short tot_len;
    unsigned short id;
    unsigned short frag_off;
    unsigned char ttl;
    unsigned char protocol;
    unsigned short check;
    unsigned int saddr;
    unsigned int daddr;
};

struct udphdr {
    unsigned short source;
    unsigned short dest;
    unsigned short len;
    unsigned short check;
};

SEC("xdp")
int xdp_filter_power(struct xdp_md *ctx) {
    void *data = (void *)(long)ctx->data;
    void *data_end = (void *)(long)ctx->data_end;
    struct ethhdr *eth = data;
    struct iphdr *ip;
    struct udphdr *udp;

    // TODO: 检查以太网头部长度
    if (data + sizeof(*eth) > data_end)
        return XDP_PASS;

    // TODO: 检查是否为 IP 协议（提示：ETH_P_IP）
    if (bpf_ntohs(eth->h_proto) != /* 填写ETH_P_IP */)
        return XDP_PASS;

    // TODO: 计算 IP 头部指针
    ip = data + sizeof(*eth);
    if ((void *)ip + sizeof(*ip) > data_end)
        return XDP_PASS;

    // TODO: 检查是否为 UDP 协议（提示：IPPROTO_UDP）
    if (ip->protocol != /* 填写IPPROTO_UDP */)
        return XDP_PASS;

    // TODO: 计算 UDP 头部指针
    udp = (void *)ip + ip->ihl * 4;
    if ((void *)udp + sizeof(*udp) > data_end)
        return XDP_PASS;

    // TODO: 只允许与电力协议端口 (ALLOW_PORT) 相关的 UDP 包通过
    //   - 请求方向：dest == ALLOW_PORT（客户端→服务器）
    //   - 回复方向：src  == ALLOW_PORT（服务器→客户端）
    //   两者满足其一即 XDP_PASS，否则 XDP_DROP
    // 提示：bpf_ntohs(udp->dest) == ALLOW_PORT || bpf_ntohs(udp->source) == ALLOW_PORT
    if (bpf_ntohs(udp->dest) == ALLOW_PORT || bpf_ntohs(udp->source) == ALLOW_PORT)
        return /* 填写XDP_PASS */;

    return /* 填写XDP_DROP */;
}

char _license[] SEC("license") = "GPL";
__u32 _version SEC("version") = 0xFFFFFFFE;
