#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Packs the C6's images for AirDrop into the P4's "c6" partition.

Two slots: esp_hosted with Wakey's addition at 0, the receiver at 2 MiB.
Each slot: "WKC6", the image's length (u32 LE), the image (firmware/src/
airdrop.rs reads them).

Usage:
  pack.py <out.bin>   writes the partition's contents, only as long as needed
  pack.py --offset    prints the partition's offset in the P4's flash
"""
import struct
import sys
from pathlib import Path

HERE = Path(__file__).parent
PARTITIONS = HERE.parent / "partitions.csv"
# Offsets in the partition: must match `C6Image` in firmware/src/airdrop.rs.
SLOTS = [
    (0x000000, HERE / "esp_hosted_slave-2.13.1-wakey2-esp32c6.bin"),
    (0x200000, HERE / "wakey_airdrop_c6-1.1.0.bin"),
]


def size_value(text):
    """A partitions.csv number: 0x..., decimal, or with a K/M suffix."""
    text = text.strip()
    scale = {"K": 1024, "M": 1024 * 1024}.get(text[-1:].upper(), 1)
    if scale != 1:
        text = text[:-1]
    return int(text, 0) * scale


def c6_partition():
    """(offset, size) of the "c6" row in partitions.csv."""
    for line in PARTITIONS.read_text().splitlines():
        fields = [f.strip() for f in line.split("#", 1)[0].split(",")]
        if fields[0] == "c6":
            return size_value(fields[3]), size_value(fields[4])
    raise SystemExit(f"pack.py: no c6 partition in {PARTITIONS}")


def main(argv):
    if len(argv) != 2:
        raise SystemExit(__doc__.strip())
    offset, size = c6_partition()
    if argv[1] == "--offset":
        print(hex(offset))
        return

    out = bytearray(b"\xff" * size)
    ends = [start for start, _ in SLOTS[1:]] + [size]
    used = 0
    for (start, path), end in zip(SLOTS, ends):
        image = path.read_bytes()
        entry = b"WKC6" + struct.pack("<I", len(image)) + image
        if start + len(entry) > end:
            raise SystemExit(f"pack.py: {path.name} ({len(image)} bytes) does not fit its slot")
        out[start : start + len(entry)] = entry
        used = start + len(entry)
    # Whole 4 KiB sectors up to the last image: the rest of the partition is
    # not written.
    Path(argv[1]).write_bytes(out[: (used + 0xFFF) & ~0xFFF])


if __name__ == "__main__":
    main(sys.argv)
