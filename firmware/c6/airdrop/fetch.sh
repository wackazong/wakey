#!/bin/sh
# Fetches the third-party sources into vendor/ at pinned commits and applies
# patches/*.patch to ESP32Drop. Run once before the first build; again after
# changing a pin or a patch (it starts from clean checkouts).
#
#   vendor/esp32drop  ESP32Drop (0BSD): AWDL + AirDrop receiver, the firmware
#   vendor/miniz      miniz 1.14 (public domain): tinfl/tdefl for the host
#                     test only; the firmware uses the C6's ROM copy
set -eu

cd "$(dirname "$0")"
mkdir -p vendor

fetch() { # dir repo commit
	rm -rf "vendor/$1"
	git clone --quiet "$2" "vendor/$1"
	git -C "vendor/$1" -c advice.detachedHead=false checkout --quiet "$3"
	echo "$1: $3"
}

fetch esp32drop https://github.com/s-iwaki-d/ESP32Drop \
	92bdc1221924ca522f3ccbebbd9353612ce5b2d7 # 2026-09-07
fetch miniz https://github.com/richgel999/miniz \
	48605fb1bd5662effe13b789d866981225e71256 # tag v114

for p in patches/*.patch; do
	[ -e "$p" ] || continue
	git -C vendor/esp32drop apply --verbose "../../$p"
done
