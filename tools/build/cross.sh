#!/bin/sh
# Exact production Buildroot SDK. No network or bindgen during production builds.
set -eu
repo=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
build=${Y2_BUILDROOT_OUTPUT:?Set Y2_BUILDROOT_OUTPUT to the production Buildroot output}
build=$(CDPATH= cd -- "$build" && pwd)
sdk=$build/host
sysroot=$($sdk/bin/arm-linux-gcc -print-sysroot)
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=$sdk/bin/arm-linux-gcc
export CC_armv7_unknown_linux_gnueabihf=$sdk/bin/arm-linux-gcc
export AR_armv7_unknown_linux_gnueabihf=$sdk/bin/arm-linux-ar
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR=$sysroot
export PKG_CONFIG_LIBDIR=$sysroot/usr/lib/pkgconfig:$sysroot/usr/share/pkgconfig
export CFLAGS_armv7_unknown_linux_gnueabihf='-mtune=cortex-a7 -mfpu=neon-vfpv4 -mfloat-abi=hard'
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS='-C target-cpu=cortex-a7 -C link-arg=-Wl,-z,relro,-z,now'
cd "$repo"
exec cargo build --workspace --release --locked --offline --target armv7-unknown-linux-gnueabihf
