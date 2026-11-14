#!/usr/bin/env python3
"""Packs the C6's images for AirDrop into the "c6" partition's contents.

Two slots: esp_hosted with Wakey's addition at 0, the receiver at 2 MiB.
Each slot: "WKC6", the image's length (u32 LE), the image (src/airdrop.rs
reads them). Usage: pack.py <out.bin>
"""
import struct
import sys
from pathlib import Path

HERE = Path(__file__).parent
SLOTS = [
    (0x000000, HERE / "esp_hosted_slave-2.13.1-wakey1-esp32c6.bin"),
    (0x200000, HERE / "wakey_airdrop_c6-1.0.0.bin"),
]
SIZE = 0x3F0000  # partitions.csv

out = bytearray(b"\xff" * SIZE)
for offset, path in SLOTS:
    image = path.read_bytes()
    entry = b"WKC6" + struct.pack("<I", len(image)) + image
    end = SLOTS[SLOTS.index((offset, path)) + 1][0] if offset != SLOTS[-1][0] else SIZE
    assert offset + len(entry) <= end, f"{path.name} does not fit its slot"
    out[offset : offset + len(entry)] = entry
# Only as long as needed: the rest of the partition stays as it is.
last = SLOTS[-1][0] + 8 + len(SLOTS[-1][1].read_bytes())
Path(sys.argv[1]).write_bytes(out[: (last + 0xFFF) & ~0xFFF])
