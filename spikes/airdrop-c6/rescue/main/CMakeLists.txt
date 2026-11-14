idf_component_register(SRCS "rescue.c"
                       PRIV_REQUIRES esp-serial-flasher esp_driver_i2c esp_driver_gpio esp_rom
                       EMBED_FILES "${CMAKE_CURRENT_LIST_DIR}/../../../../firmware/c6/esp_hosted_slave-2.13.1-esp32c6.bin")
