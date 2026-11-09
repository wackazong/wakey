#!/usr/bin/env bash
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

# shellcheck disable=SC2086
exec espflash flash \
    --chip esp32p4 \
    --bootloader "$bootloader" \
    --partition-table "$here/partitions.csv" \
    --baud 921600 \
    --monitor \
    ${ESPFLASH_ARGS:-} \
    "$elf" "$@"
