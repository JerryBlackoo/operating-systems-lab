-----BEGIN PGP SIGNED MESSAGE-----
Hash: SHA512

Format: 1.0
Source: linux-hwe-6.14
Binary: linux-hwe-6.14-headers-6.14.0-37, linux-hwe-6.14-tools-6.14.0-37, linux-hwe-6.14-cloud-tools-6.14.0-37, linux-image-unsigned-6.14.0-37-generic, linux-image-unsigned-6.14.0-37-generic-dbgsym, linux-image-6.14.0-37-generic, linux-image-6.14.0-37-generic-dbgsym, linux-modules-6.14.0-37-generic, linux-modules-extra-6.14.0-37-generic, linux-headers-6.14.0-37-generic, linux-lib-rust-6.14.0-37-generic, linux-tools-6.14.0-37-generic, linux-cloud-tools-6.14.0-37-generic, linux-buildinfo-6.14.0-37-generic, linux-modules-ipu6-6.14.0-37-generic, linux-modules-ipu7-6.14.0-37-generic, linux-modules-iwlwifi-6.14.0-37-generic, linux-modules-usbio-6.14.0-37-generic, linux-modules-vision-6.14.0-37-generic, linux-image-unsigned-6.14.0-37-generic-64k, linux-image-unsigned-6.14.0-37-generic-64k-dbgsym, linux-modules-6.14.0-37-generic-64k, linux-modules-extra-6.14.0-37-generic-64k, linux-headers-6.14.0-37-generic-64k, linux-lib-rust-6.14.0-37-generic-64k,
 linux-tools-6.14.0-37-generic-64k, linux-cloud-tools-6.14.0-37-generic-64k, linux-buildinfo-6.14.0-37-generic-64k, linux-modules-ipu6-6.14.0-37-generic-64k, linux-modules-ipu7-6.14.0-37-generic-64k, linux-modules-iwlwifi-6.14.0-37-generic-64k, linux-modules-usbio-6.14.0-37-generic-64k,
 linux-modules-vision-6.14.0-37-generic-64k
Architecture: all amd64 armhf arm64 ppc64el s390x
Version: 6.14.0-37.37~24.04.1
Maintainer: Ubuntu Kernel Team <kernel-team@lists.ubuntu.com>
Standards-Version: 3.9.4.0
Vcs-Git: git://git.launchpad.net/~ubuntu-kernel/ubuntu/+source/linux/+git/noble -b hwe-6.11
Testsuite: autopkgtest
Testsuite-Triggers: @builddeps@, build-essential, fakeroot, gcc-multilib, gdb, git, python3
Build-Depends: gcc-13, gcc-13-aarch64-linux-gnu [arm64] <cross>, gcc-13-arm-linux-gnueabihf [armhf] <cross>, gcc-13-powerpc64le-linux-gnu [ppc64el] <cross>, gcc-13-riscv64-linux-gnu [riscv64] <cross>, gcc-13-s390x-linux-gnu [s390x] <cross>, gcc-13-x86-64-linux-gnu [amd64] <cross>, autoconf <!stage1>, automake <!stage1>, bc <!stage1>, bindgen-0.65 [amd64 arm64 armhf ppc64el riscv64 s390x], bison <!stage1>, clang-18 [amd64 arm64 armhf ppc64el riscv64 s390x], cpio, curl <!stage1>, debhelper-compat (= 10), default-jdk-headless <!stage1>, dkms <!stage1>, flex <!stage1>, gawk <!stage1>, java-common <!stage1>, kmod <!stage1>, libaudit-dev <!stage1>, libcap-dev <!stage1>, libdebuginfod-dev [amd64 arm64 armhf ppc64el s390x riscv64] <!stage1>, libdw-dev <!stage1>, libelf-dev <!stage1>, libiberty-dev <!stage1>, liblzma-dev <!stage1>, libnewt-dev <!stage1>, libnuma-dev [amd64 arm64 ppc64el s390x] <!stage1>, libpci-dev <!stage1>, libssl-dev <!stage1>, libstdc++-13-dev, libtool <!stage1>, libtraceevent-dev [amd64 arm64 armhf ppc64el s390x riscv64] <!stage1>, libtracefs-dev [amd64 arm64 armhf ppc64el s390x riscv64] <!stage1>, libudev-dev <!stage1>, libunwind8-dev [amd64 arm64 armhf ppc64el] <!stage1>, makedumpfile [amd64] <!stage1>, openssl <!stage1>, pahole [amd64 arm64 armhf ppc64el s390x riscv64] | dwarves (>= 1.21) [amd64 arm64 armhf ppc64el s390x riscv64] <!stage1>, pkg-config <!stage1>, python3 <!stage1>, python3-dev <!stage1>, python3-setuptools, rsync [!i386] <!stage1>, rust-1.80-src [amd64 arm64 armhf ppc64el riscv64 s390x], rustc-1.80 [amd64 arm64 armhf ppc64el riscv64 s390x], rustfmt-1.80 [amd64 arm64 armhf ppc64el riscv64 s390x], uuid-dev <!stage1>, zstd <!stage1>
Build-Depends-Indep: asciidoc <!stage1>, bzip2 <!stage1>, python3-docutils <!stage1>, sharutils <!stage1>, xmlto <!stage1>
Package-List:
 linux-buildinfo-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-buildinfo-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-cloud-tools-6.14.0-37-generic deb devel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-cloud-tools-6.14.0-37-generic-64k deb devel optional arch=arm64 profile=!stage1
 linux-headers-6.14.0-37-generic deb devel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-headers-6.14.0-37-generic-64k deb devel optional arch=arm64 profile=!stage1
 linux-hwe-6.14-cloud-tools-6.14.0-37 deb devel optional arch=amd64,armhf profile=!stage1
 linux-hwe-6.14-headers-6.14.0-37 deb devel optional arch=all profile=!stage1
 linux-hwe-6.14-tools-6.14.0-37 deb devel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-image-6.14.0-37-generic deb kernel optional arch=armhf,ppc64el profile=!stage1
 linux-image-6.14.0-37-generic-dbgsym deb devel optional arch=armhf,ppc64el profile=!stage1
 linux-image-unsigned-6.14.0-37-generic deb kernel optional arch=amd64,arm64,s390x profile=!stage1
 linux-image-unsigned-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-image-unsigned-6.14.0-37-generic-64k-dbgsym deb devel optional arch=arm64 profile=!stage1
 linux-image-unsigned-6.14.0-37-generic-dbgsym deb devel optional arch=amd64,arm64,s390x profile=!stage1
 linux-lib-rust-6.14.0-37-generic deb devel optional arch=amd64 profile=!stage1
 linux-lib-rust-6.14.0-37-generic-64k deb devel optional arch=amd64 profile=!stage1
 linux-modules-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-extra-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-extra-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-ipu6-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-ipu6-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-ipu7-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-ipu7-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-iwlwifi-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-iwlwifi-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-usbio-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-usbio-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-modules-vision-6.14.0-37-generic deb kernel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-modules-vision-6.14.0-37-generic-64k deb kernel optional arch=arm64 profile=!stage1
 linux-tools-6.14.0-37-generic deb devel optional arch=amd64,armhf,arm64,ppc64el,s390x profile=!stage1
 linux-tools-6.14.0-37-generic-64k deb devel optional arch=arm64 profile=!stage1
Checksums-Sha1:
 d6598a246e6613bf4aedaac12b71d2065da3312c 242477923 linux-hwe-6.14_6.14.0.orig.tar.gz
 7339c4c4a0d6990d84ffda76b00714d834eec9bd 3395708 linux-hwe-6.14_6.14.0-37.37~24.04.1.diff.gz
Checksums-Sha256:
 a6dd4538509560777f0f144822d490ed1c3736b915010b4de566ab2d2a7d911e 242477923 linux-hwe-6.14_6.14.0.orig.tar.gz
 f00f1cafd09172cb5e372290148468eea461c2809797445e865d7bf1c157cc7b 3395708 linux-hwe-6.14_6.14.0-37.37~24.04.1.diff.gz
Files:
 a2a8bc6ea9a454840863ccd5eb1db451 242477923 linux-hwe-6.14_6.14.0.orig.tar.gz
 026a56c1f3a26efb1aa9ec9a9b8818e1 3395708 linux-hwe-6.14_6.14.0-37.37~24.04.1.diff.gz
Ubuntu-Compatible-Signing: ubuntu/4 pro/3

-----BEGIN PGP SIGNATURE-----

iQIzBAEBCgAdFiEE2118yvOZTjOV2k0+6Gdd7svuzqMFAmke1lAACgkQ6Gdd7svu
zqNm/BAAwXPbzLRVroKxF9JkhJJiyI+SQ+IJClA/lXm517VbgpyJG1cOIJwN5OGQ
jVJtaihdo3YRqb6gVXfvEiKkCrlBb3Kl605O67ZvLv+0KGL/N1mzBOIvC+fu/1aV
UyZiNGuObM5GOPvO9S6iMRLX+eS0ej4S1X/nVNTBZhjp8fu/9WYldKPThNwcJkMO
rcBbI7THqffHZ8hPYvCg9wpBMw1vp+i3WMcQDgK0ZCGyLBfPMmR+9A31ZLk5mz8Y
M3wFgpwiBSm9y3CkwTVOjo4avD8QDaQ77P+xh0I7odUcLQfRzstSa2uBEdFRSIkI
fKMkGX7t/jfEg40A44hE1awR7Sa73Kj7Is9pSPKoXaRHW8qw+ThjnbRjlIRpmQzy
9PJmwr9FIRH8whWn7ISMdDkeQAI7hJvicBNKdKWcXHdksRLMgcXDxWMBJJz6UnNB
xmUcQBPTqHgyM7IyHV4V8Q2k925ShXSBGXFcwkBCy4Kea5NRyd5o+t73TVCzMNG1
gyYWc01wezBaOyQVKlkO3oYu5p3SIcQyA4fH0QDKTcAIPL9MmxVF2bdR4AS000ys
u8h3HHqoNQl0smrropkmLMsS4h9S85VXkR6zG63NPZPJzUUUL4m18nu1BCMW9rBS
thtnUZQ36EnDfeSPnQO011HplobDpPt8FaY3zPcHWJiQYB55Hkw=
=DRrR
-----END PGP SIGNATURE-----
