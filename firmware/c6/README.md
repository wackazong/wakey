# ESP32-C6 firmware

`esp_hosted_slave-2.13.1-esp32c6.bin`: Espressif's esp_hosted co-processor firmware ("slave", `network_adapter`) 2.13.1 for the Tab5's ESP32-C6, SDIO, streaming mode, DIO flash. The same version as the host component in `Cargo.toml` (`espressif/esp_hosted` 2.13.1). Built 2026-11-13 from the component's `slave` project with ESP-IDF 5.5.5 and its defaults for the C6. SHA-256 `9ec988fa729eff62d09571a62464d1d3423b277eabf9779a5644b9168d1e7bea`.

The Tab5 ships with M5Stack's slave 1.4.1. It works with the host, but logs warnings at start, has a known RX stall (m5stack/M5Tab5-UserDemo#20) and reports no version at start.

## Updating a Tab5's C6

No cable: the P4 sends the image over SDIO (esp_hosted's slave OTA, `src/c6_update.rs`).

1. Flash a build with the feature: `cargo run --release --features c6-update` (or `espflash flash --monitor` on the built ELF). A `config.json` with Wi-Fi is not needed; the update runs in the first Wi-Fi session after the driver is up.
2. The log shows `c6-update: C6 runs [1, 4, 1], image [2, 13, 1] … updating`, the percentage (~12 s), `end ok`, a 5 s timeout of `activate` (slaves before 2.6 do not know it; harmless), SDIO errors as the C6 restarts, then the P4 restarts.
3. After the restart: `wifi: C6 firmware 2.13.1` and `c6-update: … nothing to do`; the "Version mismatch" and "transport size" warnings are gone.
4. Flash the normal firmware again (without the feature).

If the C6 runs the image's version already, the feature does nothing. A failure before `end ok` leaves the old firmware. The C6 has no rollback: if it does not answer after an update, reflash it by cable through header J1 "C6_ISP" (`docs/hardware.md`): 3.3 V USB-UART adapter, IO9 to GND during reset, then `esptool --chip esp32c6 write_flash 0x10000 <image>` (or M5Stack's factory image through M5Burner).

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

Keep DIO flash mode (the default): QIO images of slaves 2.1–2.8 refuse later OTAs. When the host component is updated, build the matching image, replace the file here and the name in `src/c6_update.rs`.
