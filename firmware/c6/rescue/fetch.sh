#!/bin/sh
# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Fetches Espressif's esp-serial-flasher (Apache-2.0) at a pinned commit
# into vendor/. Run once before the first build.
set -eu
cd "$(dirname "$0")"
rm -rf vendor/esp-serial-flasher
mkdir -p vendor
git clone --quiet https://github.com/espressif/esp-serial-flasher vendor/esp-serial-flasher
git -C vendor/esp-serial-flasher -c advice.detachedHead=false checkout --quiet \
	ddf3cdd8fa87f3d26b42f1b8ad94bbbb67745f50
echo "esp-serial-flasher: ddf3cdd"
