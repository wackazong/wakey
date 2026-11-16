/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * Frames to the P4 (link.h).
 */

#include "link.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "driver/uart.h"
#include "esp_log.h"
#include "esp_rom_crc.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#define LINK_UART UART_NUM_1
#define LINK_TX_GPIO 2
#define LINK_TX_BUFFER 16384
/* The link only transmits, but the driver needs an RX buffer larger than the
   hardware FIFO (128 bytes on the C6): the smallest it takes. */
#define LINK_RX_BUFFER 256
/* Header and CRC around the payload. */
#define LINK_OVERHEAD 9
/* What one uart_write_bytes() takes in the TX ring buffer beyond the bytes:
   the driver's item header plus the ring buffer's two item headers, aligned. */
#define LINK_RING_OVERHEAD 32
#define LINK_LOG_LINE 160

/* One frame's bytes stay together: one writer at a time. */
static SemaphoreHandle_t s_lock;

static void frame_header(uint8_t hdr[5], uint32_t *crc, uint8_t type, size_t len) {
  hdr[0] = 0xA5;
  hdr[1] = 0x5A;
  hdr[2] = type;
  hdr[3] = (uint8_t)len;
  hdr[4] = (uint8_t)(len >> 8);
  *crc = esp_rom_crc32_le(0, hdr + 2, 3);
}

static void put_crc(uint8_t tail[4], uint32_t crc) {
  tail[0] = (uint8_t)crc;
  tail[1] = (uint8_t)(crc >> 8);
  tail[2] = (uint8_t)(crc >> 16);
  tail[3] = (uint8_t)(crc >> 24);
}

bool link_send(uint8_t type, const void *head, size_t head_len, const void *body, size_t body_len) {
  const size_t len = head_len + body_len;
  if (len > LINK_PAYLOAD_MAX) return false;
  uint8_t hdr[5], tail[4];
  uint32_t crc;
  frame_header(hdr, &crc, type, len);
  if (head_len) crc = esp_rom_crc32_le(crc, head, head_len);
  if (body_len) crc = esp_rom_crc32_le(crc, body, body_len);
  put_crc(tail, crc);

  xSemaphoreTake(s_lock, portMAX_DELAY);
  uart_write_bytes(LINK_UART, hdr, sizeof hdr);
  if (head_len) uart_write_bytes(LINK_UART, head, head_len);
  if (body_len) uart_write_bytes(LINK_UART, body, body_len);
  uart_write_bytes(LINK_UART, tail, sizeof tail);
  xSemaphoreGive(s_lock);
  return true;
}

/* ESP_LOG's output, one line per LOG frame. Logging happens on any task (the
   Wi-Fi driver logs from its own), so a line never waits: it is dropped when
   another frame is being written (or this task is the one writing it) or the
   TX buffer cannot take the whole frame at once. */
static int log_vprintf(const char *fmt, va_list args) {
  uint8_t frame[LINK_OVERHEAD + LINK_LOG_LINE];
  char *line = (char *)frame + 5;
  int n = vsnprintf(line, LINK_LOG_LINE, fmt, args);
  if (n <= 0) return n;
  size_t len = (size_t)n < LINK_LOG_LINE ? (size_t)n : LINK_LOG_LINE - 1;
  while (len && (line[len - 1] == '\n' || line[len - 1] == '\r')) len--;
  if (!len) return n;

  uint32_t crc;
  frame_header(frame, &crc, LINK_LOG, len);
  put_crc(frame + 5 + len, esp_rom_crc32_le(crc, frame + 5, len));
  const size_t size = LINK_OVERHEAD + len;

  if (xSemaphoreTake(s_lock, 0) != pdTRUE) return n;
  size_t free_size = 0;
  if (uart_get_tx_buffer_free_size(LINK_UART, &free_size) == ESP_OK &&
      free_size >= size + LINK_RING_OVERHEAD)
    uart_write_bytes(LINK_UART, frame, size);
  xSemaphoreGive(s_lock);
  return n;
}

void link_init(void) {
  s_lock = xSemaphoreCreateMutex();
  /* Without the lock nothing can be sent. app_main has made esp_hosted the
     next boot already, so the restart after the abort lands there. */
  if (!s_lock) abort();
  const uart_config_t cfg = {
      .baud_rate = LINK_BAUD,
      .data_bits = UART_DATA_8_BITS,
      .parity = UART_PARITY_DISABLE,
      .stop_bits = UART_STOP_BITS_1,
      .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
      .source_clk = UART_SCLK_DEFAULT,
  };
  ESP_ERROR_CHECK(uart_driver_install(LINK_UART, LINK_RX_BUFFER, LINK_TX_BUFFER, 0, NULL, 0));
  ESP_ERROR_CHECK(uart_param_config(LINK_UART, &cfg));
  ESP_ERROR_CHECK(uart_set_pin(LINK_UART, LINK_TX_GPIO, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE,
                               UART_PIN_NO_CHANGE));
  esp_log_set_vprintf(log_vprintf);
}
