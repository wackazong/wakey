idf_component_register(
    SRCS "main.cpp"
    PRIV_REQUIRES esp32drop app_update esp_timer esp_rom esp_driver_uart mbedtls
)
