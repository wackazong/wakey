#!/bin/sh
# Builds esp_hosted's slave ("network_adapter") 2.13.1 for the Tab5's C6 with
# Wakey's addition (wakey_switch.c: switching to the AirDrop receiver), into
# ../esp_hosted_slave-2.13.1-wakey1-esp32c6.bin.
#
# The slave's sources come from the host component the firmware already
# fetched (firmware/target/.../managed_components/espressif__esp_hosted),
# so build the firmware once first. Uses the firmware's ESP-IDF.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
fw=$(cd "$here/../.." && pwd)
E=$fw/.embuild/espressif

src=$(find "$fw/target" -type d -path '*managed_components/espressif__esp_hosted/slave' 2>/dev/null | head -n 1)
[ -n "$src" ] || { echo "esp_hosted's sources not found: build the firmware first" >&2; exit 1; }
grep -q '^version: 2.13.1$' "$src/../idf_component.yml" ||
	{ echo "esp_hosted at $src is not 2.13.1" >&2; exit 1; }

work=$here/build
rm -rf "$work"
mkdir -p "$work"
cp -R "$src" "$work/slave"
cd "$work/slave"
patch -p1 --quiet < "$here/wakey.patch"
cp "$here/wakey_switch.c" main/

export IDF_PATH=$E/esp-idf/v5.5.5 IDF_TOOLS_PATH=$E ESP_IDF_VERSION=5.5
export IDF_PYTHON_ENV_PATH=$E/python_env/idf5.5_py3.14_env
export PATH=$IDF_PYTHON_ENV_PATH/bin:$E/tools/riscv32-esp-elf/esp-14.2.0_20260121/riscv32-esp-elf/bin:$E/tools/cmake/3.30.2/CMake.app/Contents/bin:$E/tools/ninja/1.12.1:$PATH
python "$IDF_PATH/tools/idf.py" set-target esp32c6 > "$work/build.log" 2>&1
python "$IDF_PATH/tools/idf.py" build >> "$work/build.log" 2>&1 ||
	{ tail -n 30 "$work/build.log" >&2; exit 1; }

out=$here/../esp_hosted_slave-2.13.1-wakey1-esp32c6.bin
cp build/network_adapter.bin "$out"
echo "$out: $(wc -c < "$out" | tr -d ' ') bytes, sha256 $(shasum -a 256 "$out" | cut -d' ' -f1)"
