#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/netfilter.h>
#include <linux/netfilter_ipv4.h>
#include <linux/ip.h>
#include <linux/udp.h>

#define ALLOW_PORT 40000

static struct nf_hook_ops nfho;

unsigned int hook_func(void *priv, struct sk_buff *skb, const struct nf_hook_state *state) {
    struct iphdr *ip_header;
    struct udphdr *udp_header;
    __be16 dest_port, src_port;

    if (!skb) return NF_ACCEPT;

    // TODO: 获取IP头部（提示：ip_hdr(skb)）
    ip_header = /* 填写ip_hdr调用 */;
    if (!ip_header) return NF_ACCEPT;

    // TODO: 检查是否为UDP协议，不是则放行（提示：IPPROTO_UDP）
    if (ip_header->protocol != /* 填写UDP协议号 */) return NF_ACCEPT;

    // TODO: 获取UDP头部（提示：udp_hdr(skb)）
    udp_header = /* 填写udp_hdr调用 */;
    if (!udp_header) return NF_ACCEPT;

    dest_port = udp_header->dest;
    src_port  = udp_header->source;

    // TODO: 只允许与电力协议端口 (ALLOW_PORT) 相关的 UDP 包通过
    //   - 请求方向：dest == ALLOW_PORT（客户端→服务器）
    //   - 回复方向：src  == ALLOW_PORT（服务器→客户端）
    //   两者满足其一即放行，否则丢弃
    // 提示：ntohs(dest_port) == ALLOW_PORT || ntohs(src_port) == ALLOW_PORT
    if (ntohs(dest_port) == ALLOW_PORT || ntohs(src_port) == ALLOW_PORT) {
        printk(KERN_INFO "Netfilter: allowed UDP (src=%d, dest=%d)\n",
               ntohs(src_port), ntohs(dest_port));
        return /* 填写NF_ACCEPT */;
    }
    printk(KERN_INFO "Netfilter: dropped UDP (src=%d, dest=%d)\n",
           ntohs(src_port), ntohs(dest_port));
    return /* 填写NF_DROP */;
}

static int __init power_filter_init(void) {
    // TODO: 设置钩子函数为 hook_func
    nfho.hook = /* 填写钩子函数名 */;
    // TODO: 设置钩子点为 NF_INET_LOCAL_IN（只过滤到本机的输入流量，更直观）
    nfho.hooknum = /* 填写钩子点 */;
    nfho.pf = PF_INET;
    nfho.priority = NF_IP_PRI_FIRST;

    // TODO: 注册钩子（提示：nf_register_net_hook(&init_net, &nfho)）
    /* 填写nf_register_net_hook调用 */;
    printk(KERN_INFO "Netfilter hook module loaded (allow UDP %d only, INPUT chain)\n", ALLOW_PORT);
    return 0;
}

static void __exit power_filter_exit(void) {
    // TODO: 注销钩子（提示：nf_unregister_net_hook(&init_net, &nfho)）
    /* 填写nf_unregister_net_hook调用 */;
    printk(KERN_INFO "Netfilter hook module unloaded\n");
}

module_init(power_filter_init);
module_exit(power_filter_exit);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Netfilter UDP Port Filter - allow only power protocol port");
MODULE_AUTHOR("CSCC");
MODULE_VERSION("1.0");
