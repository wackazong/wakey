# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# The Tab5's C6 as AirDrop receiver: ESP32Drop with Wakey's patches, files
# streamed to the P4 over UART (README.md).
cmake_minimum_required(VERSION 3.22)
# The P4 installs the receiver again when this differs from the C6's copy:
# bump it with every change, and name the image after it.
set(PROJECT_VER "1.1.1")
set(MINIMAL_BUILD ON)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(wakey_airdrop_c6)
