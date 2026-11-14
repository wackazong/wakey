cmake_minimum_required(VERSION 3.16)

# The version shows in the C6's log and in esp_app_desc_t.
set(PROJECT_VER "0.1.0")

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
idf_build_set_property(MINIMAL_BUILD ON)
project(wakey_airdrop_c6)
