#!/usr/bin/env bash
# Resolves the `profile: <hits> 0x<pc>` lines of a device log (see
# src/profiler.rs) to functions and prints hits per function.
#
#   scripts/profile.sh target/riscv32imafc-esp-espidf/release/ui_bench monitor.log
set -euo pipefail
elf="${1:?usage: profile.sh <elf> <log>}"
log="${2:?usage: profile.sh <elf> <log>}"
here="$(cd "$(dirname "$0")/.." && pwd)"
a2l="$(find "$here/.embuild" -name riscv32-esp-elf-addr2line -type f | head -1)"
total="$(grep -o 'profile: [0-9]* samples' "$log" | tail -1 | awk '{print $2}')"
grep -o 'profile: [0-9]* 0x[0-9a-f]*' "$log" | awk '{print $2, $3}' > /tmp/wakey_profile.$$
awk '{print $2}' /tmp/wakey_profile.$$ | "$a2l" -f -C -e "$elf" | awk 'NR % 2 == 1' > /tmp/wakey_funcs.$$
paste -d' ' <(awk '{print $1}' /tmp/wakey_profile.$$) /tmp/wakey_funcs.$$ \
    | awk -v total="$total" '{h=$1; $1=""; sum[$0]+=h} END {for (f in sum) printf "%6.1f%%  %s\n", 100*sum[f]/total, f}' \
    | sort -rn | head -40
rm -f /tmp/wakey_profile.$$ /tmp/wakey_funcs.$$
