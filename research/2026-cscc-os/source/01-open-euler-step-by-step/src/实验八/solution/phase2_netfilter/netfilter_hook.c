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

    ip_header = ip_hdr(skb);
    if (!ip_header) return NF_ACCEPT;

    if (ip_header->protocol != IPPROTO_UDP) return NF_ACCEPT;

    udp_header = udp_hdr(skb);
    if (!udp_header) return NF_ACCEPT;

    dest_port = udp_header->dest;
    src_port  = udp_header->source;

    // Allow if either source or destination port is the power protocol port.
    // This ensures both requests (dest=40000) and replies (src=40000) pass through.
    if (ntohs(dest_port) == ALLOW_PORT || ntohs(src_port) == ALLOW_PORT) {
        printk(KERN_INFO "Netfilter: allowed UDP packet (src=%d, dest=%d)\n",
               ntohs(src_port), ntohs(dest_port));
        return NF_ACCEPT;
    }

    printk(KERN_INFO "Netfilter: dropped UDP packet (src=%d, dest=%d)\n",
           ntohs(src_port), ntohs(dest_port));
    return NF_DROP;
}

static int __init power_filter_init(void) {
    nfho.hook = hook_func;
    nfho.hooknum = NF_INET_LOCAL_IN;
    nfho.pf = PF_INET;
    nfho.priority = NF_IP_PRI_FIRST;

    nf_register_net_hook(&init_net, &nfho);
    printk(KERN_INFO "Netfilter hook module loaded (allow UDP %d only, INPUT chain)\n", ALLOW_PORT);
    return 0;
}

static void __exit power_filter_exit(void) {
    nf_unregister_net_hook(&init_net, &nfho);
    printk(KERN_INFO "Netfilter hook module unloaded\n");
}

module_init(power_filter_init);
module_exit(power_filter_exit);

MODULE_LICENSE("GPL");
MODULE_DESCRIPTION("Netfilter UDP Port Filter - allow only power protocol port");
MODULE_AUTHOR("CSCC");
MODULE_VERSION("1.0");
