# ESP32-C6 firmware

The Tab5's ESP32-C6 is the P4's radio. The images here run on it; the clock installs them itself.

| Image | What | SHA-256 |
| --- | --- | --- |
| `esp_hosted_slave-2.13.1-wakey2-esp32c6.bin` | esp_hosted 2.13.1 with Wakey's addition (below) | `512289f172e651d67ea4db80a5987356ce47ac5c04ff7769848e691df220a1c6` |
| `wakey_airdrop_c6-1.1.1.bin` | the AirDrop receiver ([`airdrop/`](airdrop/README.md)) | `97ec7a642383af1b3a18ad88fb356ddcfc8de0b0b247a250c3b3f18c8411a7f9` |
| `esp_hosted_slave-2.13.1-esp32c6.bin` | Espressif's stock esp_hosted 2.13.1, for [`rescue/`](airdrop/README.md#recovery) | `8efd4a5cdd8e3e5251023625e03cfe0e448938d8c48887735213a40ef9244766` |

esp_hosted is Espressif's co-processor firmware ("slave", project `network_adapter`), here for SDIO, streaming mode, DIO flash: the same version as the host component in `Cargo.toml` (`espressif/esp_hosted` 2.13.1). All three are built with ESP-IDF 5.5.5 as reproducible builds (no build date or host paths in them): the same sources give the same bytes.

The Tab5 ships with M5Stack's slave 1.4.1. It works with the host, but logs warnings at start, has a known RX stall (m5stack/M5Tab5-UserDemo#20) and reports no version at start.

## Updating a Tab5's C6

Nothing to do: after the first Wi-Fi session of a boot the firmware installs its own esp_hosted and the AirDrop receiver if the C6 lacks them, in the background (~35 s), also over the factory 1.4.1. Wakey's esp_hosted is also replaced when its version differs from the image in the partition. The log says `c6: set up for AirDrop`.

A failed install leaves the C6 on its old firmware. The C6 has no rollback: if it does not answer after an update, reflash it by cable through header J1 "C6_ISP" (`docs/hardware.md`): 3.3 V USB-UART adapter, IO9 to GND during reset, then `esptool --chip esp32c6 write_flash 0x10000 <image>` (or M5Stack's factory image through M5Burner). Without an adapter: bridge only G9 to GND there and let the P4 reflash the C6 over SDIO with [`rescue/`](airdrop/README.md#recovery).

## AirDrop: the C6's two images

For AirDrop the C6 holds two images in its two app slots (how it switches: [architecture.md](../../docs/architecture.md#c6-images-and-airdrop)). Both go into the P4's flash partition `c6` ([`pack.py`](pack.py), written by `scripts/flash.sh`, which takes the partition's offset and size from `partitions.csv`); from there the firmware installs them on the C6 by esp_hosted's OTA (above).

- `esp_hosted_slave-2.13.1-wakey2-esp32c6.bin`: esp_hosted 2.13.1 with Wakey's addition ([`hosted/wakey_switch.c`](hosted/wakey_switch.c), [`hosted/wakey.patch`](hosted/wakey.patch)): two peer-data messages, what the other slot holds (and which esp_hosted runs) and boot it. Version string "2.13.1-wakey2" (the suffix is set in `wakey.patch`); its RPC version is 2.13.1, as the stock image's. 1,230,576 bytes.
- `wakey_airdrop_c6-1.1.1.bin`: the AirDrop receiver ([`airdrop/`](airdrop/README.md): ESP32Drop with Wakey's eight patches (`airdrop/patches`) and a streaming decoder, frames to the P4 on IO2, `airdrop/main/link.h`). 990,672 bytes of the 1,920 KiB app slot. `src/c6/mod.rs` installs the receiver again when the version in the partition differs from the C6's, so every change needs a new `PROJECT_VER` (`airdrop/CMakeLists.txt`).

Packed, the partition's contents are 3.1 MB (the receiver's slot starts at 2 MiB).

## Building

The images are built with the firmware's own ESP-IDF 5.5.5 (`firmware/.embuild/espressif`, installed by the firmware's first build). Its `export.sh` fails there (embuild installs only the tools the firmware needs), so [`env.sh`](env.sh) sets the environment from what is installed: `. firmware/c6/env.sh` (bash or zsh).

- esp_hosted: [`hosted/build.sh`](hosted/build.sh) builds Wakey's image, `hosted/build.sh --stock` the stock one. It takes the slave's sources from the esp_hosted component the firmware fetched (so build the firmware first), checks them against the hash in `firmware/components_esp32p4.lock`, pins the slave's own registry components with `hosted/dependencies.lock`, and writes `esp_hosted_slave-<version>-esp32c6.bin` here. Keep DIO flash mode (the default): QIO images of slaves 2.1–2.8 refuse later OTAs.
- The receiver: [`airdrop/README.md`](airdrop/README.md#build-and-test).

After a change: a new version (esp_hosted: the suffix in `hosted/wakey.patch`; the receiver: `PROJECT_VER`), the image here under its new name, the old one removed, the names in `pack.py` (and for the stock image in `rescue/main`), the names, sizes and SHA-256 values in this file and in `THIRD_PARTY_NOTICES.md`. When the host component is updated, build all three, and update `c6::C6_FIRMWARE` and the version in `hosted/build.sh`.

## Licenses

Wakey's own code here is MIT OR Apache-2.0, like the rest of the repository; Wakey's changes to esp_hosted are Apache-2.0 and those to ESP32Drop 0BSD, as the code they change. The images contain ESP-IDF, esp_hosted, ESP32Drop and the libraries they use: [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) lists them per image with their licenses and notices.
