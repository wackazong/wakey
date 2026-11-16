// SPDX-FileCopyrightText: 2026 Alexander Walter
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The one Arduino call ESP32Drop's receive path uses, on plain ESP-IDF.
#pragma once

#include <stdint.h>

#include "esp_timer.h"

static inline uint32_t millis(void) { return (uint32_t)(esp_timer_get_time() / 1000); }
