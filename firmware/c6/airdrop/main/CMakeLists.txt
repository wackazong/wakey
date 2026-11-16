# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
idf_component_register(SRCS "main.cpp" "link.c"
                       PRIV_REQUIRES esp32drop app_update esp_timer esp_rom esp_driver_uart esp_wifi)
