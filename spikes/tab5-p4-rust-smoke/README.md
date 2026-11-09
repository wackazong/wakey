# Tab5 P4 Rust firmware smoke test

This isolated spike asks whether a Rust-only, `no_std` firmware image can be
compiled and linked for the Tab5's ESP32-P4 with the Rust-native `esp-hal`.
It uses the generic RISC-V target and does not use ESP-IDF, C/C++, or the
Tab5 BSP. The program initializes the P4 HAL and then idles.

## Build

From this directory, with network access for the first dependency download:

```sh
cargo build --release --locked
```

Rust 1.97.0 and the `riscv32imafc-unknown-none-elf` target are pinned in
`rust-toolchain.toml`. The spike has its own Cargo workspace and lockfile; it
does not inherit the future firmware toolchain or settings from the repository
root.

## What this proves

A successful build proves that the pinned stable Rust toolchain, ESP32-P4
target, HAL crate, entry point, and linker scripts can produce a firmware ELF.
It is a compile/link proof only. It has not been flashed and does not prove
booting or runtime behavior on hardware.

`esp-hal` does not currently provide the Tab5's complete display/touch board
support. Display, touch, microSD, USB audio, sleep, and power behavior are not
tested by this spike. In particular, the architecture's former M5Stack BSP
and USB-audio component proposals are C components and are excluded by the
Rust-only project constraint.

The user has independently confirmed Rust + `espup` display/touch bring-up on
a CoreS3 Lite. That does not validate the Tab5's P4, MIPI-DSI display, or board
wiring. The next hardware test is to flash this P4 image on the Tab5, then
establish a Rust-native display and touch path and validate it on that board.

## Tab5 follow-up

- [ ] Flash this ELF to a Tab5 and capture the boot log.
- [ ] Identify Rust-native drivers for the panel and touch controller; record versions and licenses.
- [ ] Fill the display with a solid color, then read and log touch coordinates.
- [ ] Record failures and board revision before expanding to SD, audio, or sleep tests.