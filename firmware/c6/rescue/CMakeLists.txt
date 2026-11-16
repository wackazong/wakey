# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# c6-rescue: P4 firmware that reflashes the Tab5's ESP32-C6 over SDIO, for a C6
# that no longer runs esp_hosted (../airdrop/README.md#recovery).
cmake_minimum_required(VERSION 3.22)
set(PROJECT_VER "0.1.0")
set(EXTRA_COMPONENT_DIRS ${CMAKE_CURRENT_LIST_DIR}/vendor/esp-serial-flasher)
set(COMPONENTS main)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(c6_rescue)
