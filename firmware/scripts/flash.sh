#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Cargo runner: flashes an ELF with ESP-IDF's own bootloader (built for
# ESP32-P4 rev < 3, matching the Tab5) and our partition table, then opens
# the serial monitor.
#
#   cargo run --release --bin ui_bench        # via .cargo/config.toml
#   scripts/flash.sh target/riscv32imafc-esp-espidf/release/ui_bench
#
# Extra espflash options can be given in ESPFLASH_ARGS, e.g. a port:
#   ESPFLASH_ARGS="--port /dev/cu.usbmodem2101" cargo run --release --bin ui_bench
set -euo pipefail

elf="${1:?usage: flash.sh <elf>}"
shift
here="$(cd "$(dirname "$0")/.." && pwd)"
bootloader="$(dirname "$elf")/build/bootloader.bin"

if [[ ! -f "$bootloader" ]]; then
    echo "flash.sh: $bootloader not found (built by esp-idf-sys)" >&2
    exit 1
fi

# The C6's images for AirDrop into the "c6" partition (partitions.csv), only
# if the flash holds something else: 3.1 MB to write (2.2 MB of images, the
# rest the 0xFF gap before the receiver's slot), so compare MD5s first.
c6_offset=$(python3 "$here/c6/pack.py" --offset)
c6_dir="$(mktemp -d "${TMPDIR:-/tmp}/wakey-c6.XXXXXX")"
trap 'rm -rf "$c6_dir"' EXIT
c6_image="$c6_dir/c6.bin"
python3 "$here/c6/pack.py" "$c6_image"
c6_size=$(wc -c < "$c6_image" | tr -d ' ')
c6_md5=$(md5 -q "$c6_image" 2>/dev/null || md5sum "$c6_image" | cut -d' ' -f1)
# shellcheck disable=SC2086
if ! espflash checksum-md5 --chip esp32p4 ${ESPFLASH_ARGS:-} "$c6_offset" "$c6_size" 2>/dev/null | grep -qi "$c6_md5"; then
    echo "flash.sh: writing the C6 images ($c6_size bytes) at $c6_offset"
    # shellcheck disable=SC2086
    espflash write-bin --chip esp32p4 --baud 921600 ${ESPFLASH_ARGS:-} "$c6_offset" "$c6_image"
fi
rm -rf "$c6_dir"
trap - EXIT

# shellcheck disable=SC2086
exec espflash flash \
    --chip esp32p4 \
    --bootloader "$bootloader" \
    --partition-table "$here/partitions.csv" \
    --baud 921600 \
    --monitor \
    ${ESPFLASH_ARGS:-} \
    "$elf" "$@"
