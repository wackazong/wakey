/* Frames to the P4 (link.h). */

#include "link.h"

#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#include "driver/uart.h"
#include "esp_log.h"
#include "esp_rom_crc.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#define LINK_UART UART_NUM_1
#define LINK_TX_GPIO 2
#define LINK_TX_BUFFER 16384

static SemaphoreHandle_t s_lock;

void link_send(uint8_t type, const void *head, size_t head_len, const void *body, size_t body_len) {
  const size_t len = head_len + body_len;
  uint8_t hdr[5] = {0xA5, 0x5A, type, (uint8_t)len, (uint8_t)(len >> 8)};
  uint32_t crc = esp_rom_crc32_le(0, hdr + 2, 3);
  if (head_len) crc = esp_rom_crc32_le(crc, head, head_len);
  if (body_len) crc = esp_rom_crc32_le(crc, body, body_len);
  const uint8_t tail[4] = {(uint8_t)crc, (uint8_t)(crc >> 8), (uint8_t)(crc >> 16), (uint8_t)(crc >> 24)};

  xSemaphoreTake(s_lock, portMAX_DELAY);
  uart_write_bytes(LINK_UART, hdr, sizeof hdr);
  if (head_len) uart_write_bytes(LINK_UART, head, head_len);
  if (body_len) uart_write_bytes(LINK_UART, body, body_len);
  uart_write_bytes(LINK_UART, tail, sizeof tail);
  xSemaphoreGive(s_lock);
}

/* ESP_LOG's output, one line per LOG frame. Logging happens on any task; a
   line that would wait for the link is dropped rather than stall that task
   (the Wi-Fi driver logs from its own). */
static int log_vprintf(const char *fmt, va_list args) {
  char line[160];
  int n = vsnprintf(line, sizeof line, fmt, args);
  if (n <= 0) return n;
  size_t len = (size_t)n < sizeof line ? (size_t)n : sizeof line - 1;
  while (len && (line[len - 1] == '\n' || line[len - 1] == '\r')) len--;
  if (!len) return n;
  if (xSemaphoreTake(s_lock, 0) != pdTRUE) return n;
  xSemaphoreGive(s_lock);
  link_send(LINK_LOG, NULL, 0, line, len);
  return n;
}

void link_init(void) {
  s_lock = xSemaphoreCreateMutex();
  const uart_config_t cfg = {
      .baud_rate = LINK_BAUD,
      .data_bits = UART_DATA_8_BITS,
      .parity = UART_PARITY_DISABLE,
      .stop_bits = UART_STOP_BITS_1,
      .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
      .source_clk = UART_SCLK_DEFAULT,
  };
  ESP_ERROR_CHECK(uart_driver_install(LINK_UART, 256, LINK_TX_BUFFER, 0, NULL, 0));
  ESP_ERROR_CHECK(uart_param_config(LINK_UART, &cfg));
  ESP_ERROR_CHECK(uart_set_pin(LINK_UART, LINK_TX_GPIO, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE,
                               UART_PIN_NO_CHANGE));
  esp_log_set_vprintf(log_vprintf);
}
