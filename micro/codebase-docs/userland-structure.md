# userland Directory Structure Details

> Parent Project: micro
> Document Location: codebase-docs/userland-structure.md
> Related Main Document: codebase-structure.md

## Module Responsibility

The userland tree holds U-mode runtime libraries, init, applications, and the service servers that model PM/SM/VM/VFS/block/ext4/network/NVMe plus the virtio-blk storage driver. Blocks and filesystem services are exercised from the resident boot path and from standalone module demos.

## Directory Structure

```
userland/
├── apps/                   # U-mode application models (echo, future shell)
├── init/                   # Init process model and U-mode entry build
├── libs/                   # U-mode runtime: syscall wrappers and shared helpers
├── servers/
│   ├── block/              # Block service: request queue, dispatch to driver, completion and retry paths
│   ├── drivers/
│   │   └── storage/
│   │       ├── nvme/       # NVMe controller stand-in model
│   │       └── virtio_blk/ # Real virtio-blk legacy-v1/modern-v2 MMIO driver
│   ├── fs/
│   │   └── ext4/           # ext4 parser: superblock, group descriptors, extents, inode read
│   ├── net/                # UDP link stand-in model
│   ├── pm/                 # Process manager model (and U-mode build)
│   ├── sm/                 # Service manager model (and U-mode build)
│   ├── vfs/                # VFS routing model over filesystem services
│   └── vm/                 # VM service stand-in model
├── stub/                   # Ready-only service stubs used by resident boot
└── build/                  # Generated user ELF artifacts — not expanded
```

## Key Packages / Module Descriptions

- `userland/servers/block`: translates read/write requests into virtio-blk queue submissions, pumps interrupts, waits for used-ring completion, and reports timeout.
- `userland/servers/drivers/storage/virtio_blk`: transport implementation for both legacy v1 (QueuePFN/GuestPageSize) and modern v2 (QueueDesc/QueueAvail/QueueUsed, VERSION_1).
- `userland/servers/fs/ext4`: real ext4 file reader used to mount the image and read `/motd`.
- `userland/servers/vfs`: path-to-service routing used by the boot-time read.