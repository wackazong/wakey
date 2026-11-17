/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * c6-rescue: reflash the Tab5's ESP32-C6 with esp_hosted from the P4, over SDIO.
 *
 * For a C6 that no longer runs esp_hosted, so that esp_hosted's own OTA cannot reach it.
 * The C6's ROM loader speaks SDIO in download mode (joint download boot). The P4 drives
 * the C6's EN (GPIO15) and its SDIO bus, but not its IO9 boot strap: that has to be held
 * low by hand, a bridge between header J1's pins 5 (IO9) and 6 (GND).
 *
 * 1. C6 power on (IO expander B, P0), then reset into the ROM loader until it answers
 *    (it does once the bridge is in place).
 * 2. Read the C6's partition table and what each app slot holds (logged). A blank
 *    table means the standard layout; an unreadable one stops the tool.
 * 3. Write esp_hosted 2.13.1 (firmware/c6/) into the slot the bootloader starts when
 *    otadata is blank (factory, else ota_0), verified by MD5; then erase otadata.
 * 4. Wait for the bridge to be removed and let the C6 boot.
 */

#include <inttypes.h>
#include <stdio.h>
#include <string.h>

#include "driver/gpio.h"
#include "driver/i2c_master.h"
#include "esp32_sdio_port.h"
#include "esp_loader.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

static const char *TAG = "c6-rescue";

extern const uint8_t hosted_start[] asm("_binary_esp_hosted_slave_2_13_1_esp32c6_bin_start");
extern const uint8_t hosted_end[] asm("_binary_esp_hosted_slave_2_13_1_esp32c6_bin_end");

/* Tab5 wiring (docs/hardware.md) */
#define PIN_I2C_SDA GPIO_NUM_31
#define PIN_I2C_SCL GPIO_NUM_32
#define PIN_C6_EN GPIO_NUM_15   /* SOC_EXTRF_RST: low = C6 in reset */
#define PIN_C6_IO2 GPIO_NUM_14  /* not a strap pin; stands in for the boot pin */
#define IOE_B 0x44              /* PI4IOE5V6408, P0 = WLAN_PWR_EN */

/* esp_app_desc_t inside an app image: after the 24-byte image header and the first
   segment's 8-byte header. */
#define APP_DESC_OFFSET 32
#define APP_DESC_MAGIC 0xABCD5432u

static void halt(const char *why) {
  ESP_LOGE(TAG, "stopped: %s", why);
  for (;;) vTaskDelay(portMAX_DELAY);
}

/* ---- C6 power: expander B as the firmware sets it (board/ioexp.rs), P0 high ---- */

static void c6_power_on(void) {
  i2c_master_bus_config_t bus_cfg = {
      .i2c_port = -1,
      .sda_io_num = PIN_I2C_SDA,
      .scl_io_num = PIN_I2C_SCL,
      .clk_source = I2C_CLK_SRC_DEFAULT,
      .glitch_ignore_cnt = 7,
      .flags.enable_internal_pullup = true,
  };
  i2c_master_bus_handle_t bus;
  ESP_ERROR_CHECK(i2c_new_master_bus(&bus_cfg, &bus));
  i2c_device_config_t dev_cfg = {
      .dev_addr_length = I2C_ADDR_BIT_LEN_7, .device_address = IOE_B, .scl_speed_hz = 100000};
  i2c_master_dev_handle_t dev;
  ESP_ERROR_CHECK(i2c_master_bus_add_device(bus, &dev_cfg, &dev));
  /* Levels before directions: output, high-Z, pull select, pull enable, direction. */
  const uint8_t writes[][2] = {
      {0x05, 0b10000001}, {0x07, 0b00000110}, {0x0D, 0b00001000}, {0x0B, 0b00001000}, {0x03, 0b10111001}};
  for (size_t i = 0; i < sizeof writes / sizeof writes[0]; i++)
    ESP_ERROR_CHECK(i2c_master_transmit(dev, writes[i], 2, 100));
  ESP_LOGI(TAG, "C6 power on (WLAN_PWR_EN)");
  vTaskDelay(pdMS_TO_TICKS(100));
}

/* ---- the C6's partition table ---- */

struct Part {
  char label[17];
  uint8_t type, subtype;
  uint32_t offset, size;
};

static int read_partitions(esp_loader_t *loader, struct Part *out, int max) {
  static uint8_t table[0xC00];
  esp_loader_error_t err = esp_loader_flash_read(loader, table, 0x8000, sizeof table);
  if (err != ESP_LOADER_SUCCESS) {
    ESP_LOGW(TAG, "partition table not readable (error %d)", err);
    return -1;
  }
  int n = 0;
  for (size_t at = 0; at + 32 <= sizeof table && n < max; at += 32) {
    const uint8_t *e = table + at;
    if (e[0] != 0xAA || e[1] != 0x50) {
      /* 0xEBEB: the MD5 entry; 0xFFFF: the end, or a blank table at the start */
      if (at == 0 && !(e[0] == 0xFF && e[1] == 0xFF)) {
        ESP_LOGW(TAG, "no partition table at 0x8000 (%02x %02x)", e[0], e[1]);
        return -1;
      }
      break;
    }
    struct Part *p = &out[n++];
    p->type = e[2];
    p->subtype = e[3];
    memcpy(&p->offset, e + 4, 4);
    memcpy(&p->size, e + 8, 4);
    memcpy(p->label, e + 12, 16);
    p->label[16] = 0;
  }
  return n;
}

/* What an app slot holds: "network_adapter 2.13.1", "wakey_airdrop_c6 1.1.1", or empty. */
static void describe_app(esp_loader_t *loader, uint32_t offset, char *out, size_t cap) {
  uint8_t head[APP_DESC_OFFSET + 112];
  if (esp_loader_flash_read(loader, head, offset, sizeof head) != ESP_LOADER_SUCCESS) {
    snprintf(out, cap, "(read failed)");
    return;
  }
  uint32_t magic;
  memcpy(&magic, head + APP_DESC_OFFSET, 4);
  if (head[0] != 0xE9 || magic != APP_DESC_MAGIC) {
    snprintf(out, cap, "(no app)");
    return;
  }
  char version[33] = {0}, project[33] = {0};
  memcpy(version, head + APP_DESC_OFFSET + 16, 32);
  memcpy(project, head + APP_DESC_OFFSET + 48, 32);
  snprintf(out, cap, "%s %s", project, version);
}

/* ---- writing ---- */

static void write_image(esp_loader_t *loader, uint32_t offset, const uint8_t *img, uint32_t size) {
  esp_loader_flash_cfg_t cfg = {.offset = offset, .image_size = size, .block_size = 1024};
  ESP_LOGI(TAG, "erasing and writing %" PRIu32 " bytes at 0x%" PRIx32 "...", size, offset);
  if (esp_loader_flash_start(loader, &cfg) != ESP_LOADER_SUCCESS) halt("flash_start failed");
  for (uint32_t done = 0; done < size;) {
    uint32_t n = size - done < cfg.block_size ? size - done : cfg.block_size;
    if (esp_loader_flash_write(loader, &cfg, img + done, n) != ESP_LOADER_SUCCESS) halt("flash_write failed");
    done += n;
    if (done % (128 * 1024) < n || done == size)
      ESP_LOGI(TAG, "  %" PRIu32 " %%", (uint32_t)((uint64_t)done * 100 / size));
  }
  if (esp_loader_flash_finish(loader, &cfg) != ESP_LOADER_SUCCESS) halt("flash_finish (MD5) failed");
  ESP_LOGI(TAG, "written, MD5 verified");
}

void app_main(void) {
  const uint32_t hosted_len = (uint32_t)(hosted_end - hosted_start);
  ESP_LOGI(TAG, "c6-rescue: esp_hosted image %" PRIu32 " bytes", hosted_len);
  uint32_t magic = 0;
  if (hosted_len >= APP_DESC_OFFSET + 4) memcpy(&magic, hosted_start + APP_DESC_OFFSET, 4);
  if (hosted_len < 256 || hosted_start[0] != 0xE9 || magic != APP_DESC_MAGIC ||
      memcmp(hosted_start + APP_DESC_OFFSET + 48, "network_adapter", 16) != 0)
    halt("the embedded image is not esp_hosted's network_adapter");

  c6_power_on();

  esp32_sdio_port_t port = {
      .port.ops = &esp32_sdio_ops,
      .slot = SDMMC_HOST_SLOT_1,
      .max_freq_khz = SDMMC_FREQ_DEFAULT,
      .sdio_clk_pin = GPIO_NUM_12,
      .sdio_cmd_pin = GPIO_NUM_13,
      .sdio_d0_pin = GPIO_NUM_11,
      .sdio_d1_pin = GPIO_NUM_10,
      .sdio_d2_pin = GPIO_NUM_9,
      .sdio_d3_pin = GPIO_NUM_8,
      .reset_pin = PIN_C6_EN,
      .boot_pin = PIN_C6_IO2,
      .bus_width = SDIO_4BIT,
  };
  esp_loader_t loader;
  if (esp_loader_init_sdio(&loader, &port.port) != ESP_LOADER_SUCCESS) halt("SDIO host init failed");

  /* 1. Into the ROM loader. */
  for (int attempt = 1;; attempt++) {
    esp_loader_connect_args_t args = ESP_LOADER_CONNECT_DEFAULT();
    esp_loader_error_t err = esp_loader_connect(&loader, &args);
    if (err == ESP_LOADER_SUCCESS) break;
    ESP_LOGW(TAG, "attempt %d: no ROM loader on SDIO (error %d). Bridge header J1 pin 5 (IO9) "
                  "to pin 6 (GND); retrying in 3 s.", attempt, err);
    vTaskDelay(pdMS_TO_TICKS(3000));
  }
  target_chip_t chip = esp_loader_get_target(&loader);
  ESP_LOGI(TAG, "connected to the C6's ROM loader (chip id %d)", (int)chip);
  if (chip != ESP32C6_CHIP) halt("the target is not an ESP32-C6");
  uint32_t flash_size = 0;
  if (esp_loader_flash_detect_size(&loader, &flash_size) == ESP_LOADER_SUCCESS)
    ESP_LOGI(TAG, "C6 flash: %" PRIu32 " MB", flash_size >> 20);

  /* 2. What is there. */
  struct Part parts[16];
  int n = read_partitions(&loader, parts, 16);
  /* Unreadable: writing at a guessed offset could destroy what is there. Blank
     (n == 0): nothing to lose, and the standard layout is the best guess. */
  if (n < 0) halt("the C6's partition table cannot be read");
  uint32_t otadata_off = 0xD000, otadata_size = 0x2000, app_off = 0x10000, app_size = 0;
  if (n > 0) {
    int factory = -1, ota0 = -1, otadata = -1;
    for (int i = 0; i < n; i++) {
      char what[80] = "";
      if (parts[i].type == 0) describe_app(&loader, parts[i].offset, what, sizeof what);
      ESP_LOGI(TAG, "  %-16s type %u/%02x at 0x%06" PRIx32 ", %" PRIu32 " KiB %s", parts[i].label,
               parts[i].type, parts[i].subtype, parts[i].offset, parts[i].size >> 10, what);
      if (parts[i].type == 0 && parts[i].subtype == 0x00) factory = i;
      if (parts[i].type == 0 && parts[i].subtype == 0x10) ota0 = i;
      if (parts[i].type == 1 && parts[i].subtype == 0x00) otadata = i;
    }
    int app = factory >= 0 ? factory : ota0;
    if (app < 0) halt("no factory or ota_0 partition");
    app_off = parts[app].offset;
    app_size = parts[app].size;
    if (otadata >= 0) {
      otadata_off = parts[otadata].offset;
      otadata_size = parts[otadata].size;
    } else {
      otadata_size = 0;
    }
  } else {
    ESP_LOGW(TAG, "using the standard layout: otadata 0xd000 (8 KiB), ota_0 0x10000");
  }
  if (app_size && hosted_len > app_size) halt("esp_hosted does not fit the slot");

  /* 3. esp_hosted into the slot booted with blank otadata, then blank otadata. */
  write_image(&loader, app_off, hosted_start, hosted_len);
  if (otadata_size) {
    ESP_LOGI(TAG, "erasing otadata at 0x%" PRIx32 " (%" PRIu32 " KiB)", otadata_off, otadata_size >> 10);
    if (esp_loader_flash_erase_region(&loader, otadata_off, otadata_size) != ESP_LOADER_SUCCESS)
      halt("erasing otadata failed");
  }
  char what[80];
  describe_app(&loader, app_off, what, sizeof what);
  ESP_LOGI(TAG, "slot at 0x%" PRIx32 " now holds: %s", app_off, what);

  /* 4. Until the bridge is gone, a reset lands in the ROM loader again. */
  ESP_LOGI(TAG, "DONE. Remove the bridge between J1 pins 5 and 6 now.");
  for (;;) {
    vTaskDelay(pdMS_TO_TICKS(3000));
    esp_loader_connect_args_t args = {.sync_timeout = 100, .trials = 2};
    if (esp_loader_connect(&loader, &args) != ESP_LOADER_SUCCESS) break;
    ESP_LOGI(TAG, "the C6 still starts its ROM loader: remove the bridge (J1 pins 5-6)");
  }
  esp_loader_reset_target(&loader);
  ESP_LOGI(TAG, "bridge removed, C6 reset: it boots esp_hosted. Flash the clock firmware "
                "(cargo run --release --bin wakey); its log shows 'c6: esp_hosted [2, 13, 1]'.");
  vTaskDelay(portMAX_DELAY);
}
