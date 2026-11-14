# The Tab5's C6 as AirDrop receiver: ESP32Drop with the spike's patches,
# files streamed to the P4 over UART (README.md).
cmake_minimum_required(VERSION 3.22)
set(PROJECT_VER "1.0.0")
set(MINIMAL_BUILD ON)
include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(wakey_airdrop_c6)
