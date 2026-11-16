# ESP32-C6 firmware

`esp_hosted_slave-2.13.1-esp32c6.bin`: Espressif's stock esp_hosted co-processor firmware ("slave", `network_adapter`) 2.13.1 for the Tab5's ESP32-C6, SDIO, streaming mode, DIO flash. The same version as the host component in `Cargo.toml` (`espressif/esp_hosted` 2.13.1). Built 2026-11-13 from the component's `slave` project with ESP-IDF 5.5.5 and its defaults for the C6. Used by [`rescue/`](airdrop/README.md#recovery), which writes it over SDIO through the C6's ROM loader; the clock itself installs Wakey's variant (below). SHA-256 `9ec988fa729eff62d09571a62464d1d3423b277eabf9779a5644b9168d1e7bea`.

The Tab5 ships with M5Stack's slave 1.4.1. It works with the host, but logs warnings at start, has a known RX stall (m5stack/M5Tab5-UserDemo#20) and reports no version at start.

## Updating a Tab5's C6

Nothing to do: after the first Wi-Fi session of a boot the firmware installs its own esp_hosted (2.13.1-wakey1, below) and the AirDrop receiver if the C6 lacks them, in the background (~35 s), also over the factory 1.4.1 (tested 2026-11-15). The log says `airdrop: the C6 is set up for AirDrop`.

A failed install leaves the C6 on its old firmware. The C6 has no rollback: if it does not answer after an update, reflash it by cable through header J1 "C6_ISP" (`docs/hardware.md`): 3.3 V USB-UART adapter, IO9 to GND during reset, then `esptool --chip esp32c6 write_flash 0x10000 <image>` (or M5Stack's factory image through M5Burner). Without an adapter: bridge only G9 to GND there and let the P4 reflash the C6 over SDIO with [`rescue/`](airdrop/README.md#recovery) (used once, 2026-11-13).

## Building the image

From the host component's sources (`target/…/out/managed_components/espressif__esp_hosted/slave`, copied elsewhere), with the project's ESP-IDF in `firmware/.embuild/espressif`. Its `export.sh` fails (embuild installs only the tools the firmware needs), so set the environment by hand:

```sh
E=firmware/.embuild/espressif   # absolute path
export IDF_PATH=$E/esp-idf/v5.5.5 IDF_TOOLS_PATH=$E ESP_IDF_VERSION=5.5
export IDF_PYTHON_ENV_PATH=$E/python_env/idf5.5_py3.14_env
export PATH=$IDF_PYTHON_ENV_PATH/bin:$E/tools/riscv32-esp-elf/esp-14.2.0_20260121/riscv32-esp-elf/bin:$E/tools/cmake/3.30.2/CMake.app/Contents/bin:$E/tools/ninja/1.12.1:$PATH
python $IDF_PATH/tools/idf.py set-target esp32c6
python $IDF_PATH/tools/idf.py build    # build/network_adapter.bin
```

Keep DIO flash mode (the default): QIO images of slaves 2.1–2.8 refuse later OTAs. When the host component is updated, build the matching images (this one and Wakey's, `hosted/build.sh`), replace the files here, their names in `rescue/` and `pack.py`, and `net::C6_FIRMWARE`.

## AirDrop: the C6's other two images

For AirDrop the C6 holds two images in its two app slots (how it switches: [architecture.md](../../docs/architecture.md#airdrop)). Both go into the P4's flash partition `c6` (`pack.py`, written by `scripts/flash.sh`); the firmware installs them on the C6 by esp_hosted's OTA when they are missing, in the background after the first Wi-Fi session of a boot.

- `esp_hosted_slave-2.13.1-wakey1-esp32c6.bin`: esp_hosted 2.13.1 with Wakey's addition ([`hosted/wakey_switch.c`](hosted/wakey_switch.c), [`hosted/wakey.patch`](hosted/wakey.patch)): two peer-data messages, what the other slot holds and boot it. Version string "2.13.1-wakey1"; its RPC version is 2.13.1, as the stock image. Build: `hosted/build.sh` (copies the slave's sources from the firmware's managed components, patches, builds). SHA-256 `3cf3f49609e0fe41728396e2f0a9d607b6489c5ff817188384993ae6e5d3ed1f`.
- `wakey_airdrop_c6-1.0.0.bin`: the AirDrop receiver ([`airdrop/`](airdrop/main/main.cpp): ESP32Drop with Wakey's ten patches (`airdrop/patches`) and its streaming decoder, frames to the P4 on IO2, `airdrop/main/link.h`). Build: in `airdrop/`, `./fetch.sh`, `./test/run.sh`, `. ./env.sh`, `python "$IDF_PATH/tools/idf.py" set-target esp32c6`, `python "$IDF_PATH/tools/idf.py" build`, then copy `build/wakey_airdrop_c6.bin` here with its version in the name (`src/airdrop.rs` installs the receiver again when the version in the partition differs from the C6's). SHA-256 `e44d08b2f72ba0d5057ce36ce2dd7e5c69c6ff5a41c12a17820c86e1cc0b5f65`.

After a change: `pack.py` and `scripts/flash.sh` need the new file names. Recovery if a receiver crashes before it makes esp_hosted the next boot: [`rescue/`](airdrop/README.md#recovery) (G9 bridged to GND, no adapter).

