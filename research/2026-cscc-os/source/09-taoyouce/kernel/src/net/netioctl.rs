//! Network device ioctl implementations
//!
//! Implements socket-level ioctls per Linux net/core/dev_ioctl.c:
//!   SIOCGIFCONF, SIOCGIFADDR, SIOCGIFFLAGS, SIOCGIFNETMASK,
//!   SIOCGIFHWADDR, SIOCGIFINDEX, SIOCGIFMTU, SIOCGIFBRDADDR,
//!   SIOCGIFDSTADDR, SIOCGIFNAME, SIOCGIFMAP, SIOCINQ, SIOCOUTQ
//!
//! The virtual network configuration:
//!   lo:   127.0.0.1/8, flags=IFF_UP|IFF_LOOPBACK|IFF_RUNNING, mtu=65536, index=1
//!   eth0: 10.0.2.15/24, flags=IFF_UP|IFF_BROADCAST|IFF_RUNNING|IFF_MULTICAST, mtu=1500, index=2



// Linux ioctl numbers (from asm-generic/sockios.h and linux/sockios.h)
pub const SIOCGIFNAME:    usize = 0x8910;
pub const SIOCGIFCONF:    usize = 0x8912;
pub const SIOCGIFFLAGS:   usize = 0x8913;
pub const SIOCSIFFLAGS:   usize = 0x8914;
pub const SIOCGIFADDR:    usize = 0x8915;
pub const SIOCSIFADDR:    usize = 0x8916;
pub const SIOCGIFDSTADDR: usize = 0x8917;
pub const SIOCGIFBRDADDR: usize = 0x8919;
pub const SIOCGIFNETMASK: usize = 0x891B;
pub const SIOCGIFHWADDR:  usize = 0x8927;
pub const SIOCGIFMTU:     usize = 0x8921;
pub const SIOCSIFMTU:     usize = 0x8922;
pub const SIOCGIFINDEX:   usize = 0x8933;
pub const SIOCGIFMAP:     usize = 0x8970;
pub const SIOCGIFTXQLEN:  usize = 0x8942;

// from asm-generic/sockios.h
pub const SIOCINQ:  usize = 0x541B; // same as FIONREAD
pub const SIOCOUTQ: usize = 0x5411; // TIOCOUTQ

// Interface flags (linux/if.h)
const IFF_UP: u16         = 0x1;
const IFF_BROADCAST: u16  = 0x2;
const IFF_LOOPBACK: u16   = 0x8;
const IFF_RUNNING: u16    = 0x40;
const IFF_MULTICAST: u16  = 0x1000;

// ARPHRD_* (linux/if_arp.h)
const ARPHRD_LOOPBACK: u16 = 772;
const ARPHRD_ETHER: u16    = 1;

/// Virtual interface definition
struct VirtIface {
    name: &'static str,
    index: i32,
    flags: u16,
    mtu: i32,
    ip: [u8; 4],       // network byte order
    netmask: [u8; 4],  // network byte order
    broadcast: [u8; 4], // network byte order
    hwaddr: [u8; 6],
    hwtype: u16,
    txqlen: i32,
}

static INTERFACES: &[VirtIface] = &[
    VirtIface {
        name: "lo",
        index: 1,
        flags: IFF_UP | IFF_LOOPBACK | IFF_RUNNING,
        mtu: 65536,
        ip: [127, 0, 0, 1],
        netmask: [255, 0, 0, 0],
        broadcast: [0, 0, 0, 0],
        hwaddr: [0; 6],
        hwtype: ARPHRD_LOOPBACK,
        txqlen: 1000,
    },
    VirtIface {
        name: "eth0",
        index: 2,
        flags: IFF_UP | IFF_BROADCAST | IFF_RUNNING | IFF_MULTICAST,
        mtu: 1500,
        ip: [10, 0, 2, 15],
        netmask: [255, 255, 255, 0],
        broadcast: [10, 0, 2, 255],
        hwaddr: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        hwtype: ARPHRD_ETHER,
        txqlen: 1000,
    },
];

fn find_iface_by_name(name: &[u8]) -> Option<&'static VirtIface> {
    // The interface name occupies the bytes up to the first NUL; everything
    // after it is uninitialized caller stack garbage (struct ifreq.ifr_name is
    // a fixed 16-byte field). Validate only the NUL-terminated prefix as UTF-8
    // — validating the full 16 bytes wrongly rejects a valid name when the
    // trailing garbage is not valid UTF-8 (arch-dependent SIOCGIFINDEX ENODEV).
    let nul = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let name_str = core::str::from_utf8(&name[..nul]).ok()?;
    INTERFACES.iter().find(|iface| iface.name == name_str)
}

fn find_iface_by_index(idx: i32) -> Option<&'static VirtIface> {
    INTERFACES.iter().find(|iface| iface.index == idx)
}

/// Build a sockaddr_in from IPv4 address bytes.
/// struct sockaddr_in { u16 sin_family; u16 sin_port; u32 sin_addr; u8 pad[8]; }
fn build_sockaddr_in(ip: &[u8; 4]) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[0] = 2;  // AF_INET low byte
    addr[1] = 0;  // AF_INET high byte
    // port = 0
    addr[4] = ip[0];
    addr[5] = ip[1];
    addr[6] = ip[2];
    addr[7] = ip[3];
    addr
}

/// Handle socket-level network ioctls.
/// Returns Some(retval) if handled, None if not a network ioctl.
pub fn handle_net_ioctl(request: usize, arg: usize) -> Option<isize> {
    match request {
        SIOCGIFCONF => Some(handle_siocgifconf(arg)),
        SIOCGIFFLAGS => Some(handle_ifreq_read(arg, |iface, buf| {
            // ifr_flags at offset 16..18
            buf[16..18].copy_from_slice(&iface.flags.to_le_bytes());
        })),
        SIOCGIFADDR => Some(handle_ifreq_read(arg, |iface, buf| {
            let addr = build_sockaddr_in(&iface.ip);
            buf[16..32].copy_from_slice(&addr);
        })),
        SIOCGIFNETMASK => Some(handle_ifreq_read(arg, |iface, buf| {
            let addr = build_sockaddr_in(&iface.netmask);
            buf[16..32].copy_from_slice(&addr);
        })),
        SIOCGIFBRDADDR => Some(handle_ifreq_read(arg, |iface, buf| {
            let addr = build_sockaddr_in(&iface.broadcast);
            buf[16..32].copy_from_slice(&addr);
        })),
        SIOCGIFDSTADDR => Some(handle_ifreq_read(arg, |iface, buf| {
            let addr = build_sockaddr_in(&iface.ip);
            buf[16..32].copy_from_slice(&addr);
        })),
        SIOCGIFHWADDR => Some(handle_ifreq_read(arg, |iface, buf| {
            // struct sockaddr { sa_family_t sa_family; char sa_data[14]; }
            buf[16..18].copy_from_slice(&iface.hwtype.to_le_bytes());
            buf[18..24].copy_from_slice(&iface.hwaddr);
        })),
        SIOCGIFMTU => Some(handle_ifreq_read(arg, |iface, buf| {
            buf[16..20].copy_from_slice(&iface.mtu.to_le_bytes());
        })),
        SIOCGIFINDEX => Some(handle_ifreq_read(arg, |iface, buf| {
            buf[16..20].copy_from_slice(&iface.index.to_le_bytes());
        })),
        SIOCGIFTXQLEN => Some(handle_ifreq_read(arg, |iface, buf| {
            buf[16..20].copy_from_slice(&iface.txqlen.to_le_bytes());
        })),
        SIOCGIFNAME => Some(handle_siocgifname(arg)),
        SIOCGIFMAP => Some(handle_ifreq_read(arg, |_iface, buf| {
            // struct ifmap { unsigned long mem_start, mem_end, base_addr; ... }
            // All zeros for virtual interfaces
            for b in buf[16..32].iter_mut() { *b = 0; }
        })),
        SIOCSIFFLAGS | SIOCSIFADDR | SIOCSIFMTU => {
            // Write operations: accept silently (we don't actually reconfigure)
            Some(0)
        }
        _ => None,
    }
}

// Safe wrappers around user-space memory access.
//
// These MUST go through the page-table-walking copy helpers
// (`read_user_buffer` / `write_user_buffer`), not a raw
// `copy_nonoverlapping` on the user VA. A raw access dereferences the user
// virtual address directly from S-mode: on RISC-V that requires the SUM bit
// and faults (looping) on a not-yet-resident user page, hanging the ioctl;
// on LoongArch it happens to work, which is why this bug was arch-specific.
// The buffer helpers translate via the page table, demand-fault lazy pages,
// and copy through the kernel direct map — matching Linux copy_{from,to}_user.
fn read_user(ptr: usize, buf: &mut [u8]) -> bool {
    if ptr == 0 { return false; }
    match crate::syscall::user::read_user_buffer(ptr as *const u8, buf.len()) {
        Ok(data) => {
            buf.copy_from_slice(&data);
            true
        }
        Err(_) => false,
    }
}

fn write_user(ptr: usize, buf: &[u8]) -> bool {
    if ptr == 0 { return false; }
    crate::syscall::user::write_user_buffer(ptr as *mut u8, buf).is_ok()
}

/// SIOCGIFCONF: fill struct ifconf { int ifc_len; union { char* buf; struct ifreq* req; }; }
/// Layout: [4 bytes ifc_len][4 bytes pad on 64-bit][8 bytes ifc_buf pointer]
/// Each ifreq is 32 bytes (IFNAMSIZ=16 name + 16 sockaddr)
fn handle_siocgifconf(arg: usize) -> isize {
    if arg == 0 { return -14; }

    // Read the ifconf structure (16 bytes on 64-bit: 4 len + 4 pad + 8 ptr)
    let mut ifconf_data = [0u8; 16];
    if !read_user(arg, &mut ifconf_data) { return -14; }

    let ifc_len = i32::from_le_bytes([ifconf_data[0], ifconf_data[1], ifconf_data[2], ifconf_data[3]]);
    let ifc_buf_ptr = u64::from_le_bytes([
        ifconf_data[8], ifconf_data[9], ifconf_data[10], ifconf_data[11],
        ifconf_data[12], ifconf_data[13], ifconf_data[14], ifconf_data[15],
    ]) as usize;

    if ifc_buf_ptr == 0 {
        // If buf is NULL, just return total required length
        let needed = (INTERFACES.len() * 32) as i32;
        write_user(arg, &needed.to_le_bytes());
        return 0;
    }

    let max_entries = (ifc_len as usize) / 32;
    let mut written = 0usize;
    for (i, iface) in INTERFACES.iter().enumerate() {
        if i >= max_entries { break; }
        let mut ifreq = [0u8; 32];
        // Copy interface name (IFNAMSIZ=16)
        let name_bytes = iface.name.as_bytes();
        let copy_len = name_bytes.len().min(15);
        ifreq[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        // sockaddr_in at offset 16
        let addr = build_sockaddr_in(&iface.ip);
        ifreq[16..32].copy_from_slice(&addr);
        if !write_user(ifc_buf_ptr + i * 32, &ifreq) { break; }
        written += 32;
    }

    // Write back actual bytes written to ifc_len
    write_user(arg, &(written as i32).to_le_bytes());
    0
}

/// Generic ifreq read handler: read name from user, find interface, apply callback, write back.
fn handle_ifreq_read(arg: usize, fill: impl FnOnce(&VirtIface, &mut [u8; 32])) -> isize {
    if arg == 0 { return -14; }

    let mut data = [0u8; 32];
    if !read_user(arg, &mut data) { return -14; }

    let iface = match find_iface_by_name(&data[..16]) {
        Some(i) => i,
        None => return -19, // -ENODEV
    };

    let mut buf = [0u8; 32];
    buf[..16].copy_from_slice(&data[..16]); // preserve name
    fill(iface, &mut buf);

    if !write_user(arg, &buf) { return -14; }
    0
}

/// SIOCGIFNAME: given index in ifr_ifindex, return interface name
fn handle_siocgifname(arg: usize) -> isize {
    if arg == 0 { return -14; }

    let mut data = [0u8; 32];
    if !read_user(arg, &mut data) { return -14; }

    let ifindex = i32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    let iface = match find_iface_by_index(ifindex) {
        Some(i) => i,
        None => return -19, // -ENODEV
    };

    let mut buf = [0u8; 32];
    let name_bytes = iface.name.as_bytes();
    let copy_len = name_bytes.len().min(15);
    buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
    buf[16..].copy_from_slice(&data[16..32]);

    if !write_user(arg, &buf) { return -14; }
    0
}
