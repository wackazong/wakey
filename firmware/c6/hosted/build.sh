#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Builds esp_hosted's slave ("network_adapter") 2.13.1 for the Tab5's C6:
#
#   build.sh           with Wakey's addition (wakey.patch, wakey_switch.c), into
#                      ../esp_hosted_slave-<version>-esp32c6.bin, the version
#                      being the one wakey.patch sets (2.13.1-wakeyN)
#   build.sh --stock   unchanged, into ../esp_hosted_slave-2.13.1-esp32c6.bin
#                      (the image rescue/ embeds)
#
# The slave's sources come from the esp_hosted component the firmware fetched
# (firmware/target/.../managed_components/espressif__esp_hosted), checked
# against its hash in firmware/components_esp32p4.lock: build the firmware
# once first. The registry components the slave pulls in are pinned by
# dependencies.lock here. Uses the firmware's ESP-IDF (../env.sh).
set -euo pipefail

stock=false
case "${1:-}" in
"") ;;
--stock) stock=true ;;
*)
	echo "usage: build.sh [--stock]" >&2
	exit 2
	;;
esac

here=$(cd "$(dirname "$0")" && pwd)
fw=$(cd "$here/../.." && pwd)
# shellcheck source-path=SCRIPTDIR source=../env.sh
. "$here/../env.sh"

# Every copy of the component the firmware's builds fetched has the same hash
# (the lock pins it); take the first in sorted order and verify it.
src=$(find "$fw/target" -type d -path '*/managed_components/espressif__esp_hosted' 2>/dev/null | sort | head -n 1)
[ -n "$src" ] || {
	echo "esp_hosted's sources not found: build the firmware first" >&2
	exit 1
}
python - "$src" "$fw/components_esp32p4.lock" <<'EOF' || exit 1
import sys
import yaml
from idf_component_tools.hash_tools.validate import (
    validate_hash_eq_hashfile,
    validate_hashfile_eq_hashdir,
)

src, lock = sys.argv[1:]
entry = yaml.safe_load(open(lock))["dependencies"]["espressif/esp_hosted"]
if entry["version"] != "2.13.1":
    sys.exit(f"components_esp32p4.lock pins esp_hosted {entry['version']}, this script 2.13.1")
try:
    validate_hash_eq_hashfile(src, entry["component_hash"])
    validate_hashfile_eq_hashdir(src)
except Exception as e:
    sys.exit(f"esp_hosted at {src} does not match components_esp32p4.lock: {e}")
EOF

work=$here/build
rm -rf "$work"
mkdir -p "$work"
cp -R "$src/slave" "$work/slave"
cd "$work/slave"
if ! $stock; then
	patch -p1 --quiet < "$here/wakey.patch"
	cp "$here/wakey_switch.c" main/
fi
# The lock names the local cmd_system component by its absolute path.
sed "s|\${IDF_PATH}|$IDF_PATH|" "$here/dependencies.lock" > dependencies.lock
cp dependencies.lock "$work/dependencies.lock.expected"
# The same image from the same sources: no build date or host paths in it.
cat >> sdkconfig.defaults <<'EOF'

CONFIG_APP_COMPILE_TIME_DATE=n
CONFIG_APP_REPRODUCIBLE_BUILD=y
EOF

log=$work/build.log
fail() {
	echo "build.sh: $1 failed; the end of $log:" >&2
	tail -n 30 "$log" >&2
	exit 1
}
python "$IDF_PATH/tools/idf.py" set-target esp32c6 > "$log" 2>&1 || fail "set-target"
python "$IDF_PATH/tools/idf.py" build >> "$log" 2>&1 || fail "build"
cmp -s dependencies.lock "$work/dependencies.lock.expected" || {
	echo "build.sh: the component manager resolved other versions than dependencies.lock:" >&2
	diff "$work/dependencies.lock.expected" dependencies.lock >&2 || true
	exit 1
}

# The version from the image's app description (esp_app_desc_t at 32, version at +16).
version=$(python -c 'import sys; d = open(sys.argv[1], "rb").read(32 + 48); print(d[48:80].split(b"\0")[0].decode())' build/network_adapter.bin)
out=$here/../esp_hosted_slave-$version-esp32c6.bin
cp build/network_adapter.bin "$out"
echo "$out: $(wc -c < "$out" | tr -d ' ') bytes, sha256 $(shasum -a 256 "$out" | cut -d' ' -f1)"
