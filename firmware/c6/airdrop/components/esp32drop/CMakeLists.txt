# ESP32Drop's receiver (vendor/esp32drop, from ./fetch.sh with patches/),
# built for the ESP32-C6 with ESP-IDF instead of the Arduino core, plus the
# streaming decoder (stream/) that lets it receive without PSRAM.

set(src "${CMAKE_CURRENT_LIST_DIR}/../../vendor/esp32drop/ESP32Drop/src")
if(NOT EXISTS "${src}/ESP32Drop.h")
    message(FATAL_ERROR "ESP32Drop is missing: run ./fetch.sh")
endif()

idf_component_register(
    SRCS "${src}/awdl/port/awdl_port_esp32.cpp"
         "${src}/airdrop/port/ad_port_esp32.cpp"
         "stream/ad_stream.c"
    INCLUDE_DIRS "${src}" "stream" "shim"
    REQUIRES esp_wifi esp_netif esp_event lwip mbedtls esp_timer esp_rom
    PRIV_REQUIRES nvs_flash
)

# Uploads are decoded while they arrive and handed out through ad_on_stream()
# (patches/0002); public, because it selects declarations in the API header.
target_compile_definitions(${COMPONENT_LIB} PUBLIC AD_RX_STREAM)
target_compile_definitions(${COMPONENT_LIB} PRIVATE
    # No PSRAM: what is left of ESP32Drop's bulk buffers (diagnostics, the
    # transmit retry frame) comes from internal RAM.
    "AD_BULK_CAPS=(MALLOC_CAP_INTERNAL|MALLOC_CAP_8BIT)"
    "AD_RX_HEADROOM=(64u*1024)"
    # AWDL on the unassociated station: the C6 ACKs AWDL's unicast frames there,
    # not on the soft-AP (patches/0004).
    "AWDL_WIFI_IF=WIFI_IF_STA"
)
# Third-party code: keep its warnings visible, but not fatal (ESP-IDF makes
# these two errors; the Arduino core does not).
target_compile_options(${COMPONENT_LIB} PRIVATE
    -Wno-error=comment
    -Wno-error=maybe-uninitialized
)
