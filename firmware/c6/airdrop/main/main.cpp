// SPDX-FileCopyrightText: 2026 Alexander Walter
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// The Tab5's C6 as AirDrop receiver ("Wakey"), the role next to esp_hosted.
//
// esp_hosted (../hosted) starts this image on the P4's request; first thing
// it makes esp_hosted the next boot again, so a power cycle of the C6 ends
// AirDrop. Received files are decoded while they arrive (ESP32Drop with
// Wakey's patches) and go to the P4 as frames on IO2 (link.h): the P4
// writes them to the card and decides what to keep.

#include <cinttypes>
#include <cstring>

#include "ESP32Drop.h"
#include "esp_app_desc.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "link.h"

namespace {

const char *const TAG = "airdrop";

// The name on the sender's share sheet.
constexpr const char *kName = "Wakey";
// esp_hosted's project name.
constexpr const char *kHostedProject = "network_adapter";

// Announce 2.4 GHz only (ESP32Drop's default says dualband): the iPhone then
// delivered 123-136 instead of 79-98 kB/s (README.md#speed).
constexpr uint16_t kDpsFlags = AWDL_DPS_DEFAULT & ~AWDL_DPS_DUALBAND;

// What boot_hosted_next() did, logged once the link is up.
struct {
  bool found;
  esp_err_t err;
  char label[17];
  esp_app_desc_t desc;
} g_boot_next;

// Makes the other slot, if it holds esp_hosted, the next boot. Runs before
// anything else, so that no failure later can leave the C6 booting this image
// again and again. Not inlined: it has the stack to itself
// (esp_ota_set_boot_partition hashes the image deep in the stack; an overflow
// here leaves the C6 in a boot loop).
__attribute__((noinline)) void boot_hosted_next() {
  const esp_partition_t *other = esp_ota_get_next_update_partition(nullptr);
  esp_app_desc_t &desc = g_boot_next.desc;
  if (other == nullptr || esp_ota_get_partition_description(other, &desc) != ESP_OK ||
      strcmp(desc.project_name, kHostedProject) != 0) {
    return;
  }
  g_boot_next.found = true;
  strlcpy(g_boot_next.label, other->label, sizeof g_boot_next.label);
  g_boot_next.err = esp_ota_set_boot_partition(other);
}

void log_boot_next() {
  if (!g_boot_next.found) {
    ESP_LOGE(TAG, "no esp_hosted in the other slot: boot slot unchanged");
    return;
  }
  ESP_LOGI(TAG, "next boot: %s %s from %s (%s)", g_boot_next.desc.project_name, g_boot_next.desc.version,
           g_boot_next.label, esp_err_to_name(g_boot_next.err));
}

// The raw frames' rate (AWDL's action frames, our TCP ACKs): 11g 24 Mbit/s
// instead of the default 1 Mbit/s (~1 ms of a channel-6 slot per ACK). HT
// rates wedge the C6's transmitter.
void set_tx_rate() {
  wifi_tx_rate_config_t config = {
      .phymode = WIFI_PHY_MODE_11G,
      .rate = WIFI_PHY_RATE_24M,
      .ersu = false,
      .dcm = false,
  };
  esp_err_t err = esp_wifi_config_80211_tx(WIFI_IF_STA, &config);
  if (err != ESP_OK) ESP_LOGW(TAG, "raw TX rate: %s", esp_err_to_name(err));
}

// ---- the stream sink: runs on ESP32Drop's TLS task ----

uint32_t g_offset;  // in the current file

bool on_begin(void *, const char *name, uint32_t size) {
  g_offset = 0;
  uint8_t head[4] = {(uint8_t)size, (uint8_t)(size >> 8), (uint8_t)(size >> 16), (uint8_t)(size >> 24)};
  // Names are at most 255 bytes (ad_stream.h): the frame always fits.
  return link_send(LINK_BEGIN, head, sizeof head, name, strlen(name));
}

bool on_data(void *, const uint8_t *data, size_t len) {
  while (len) {
    size_t n = len < LINK_DATA_MAX ? len : LINK_DATA_MAX;
    uint8_t head[4] = {(uint8_t)g_offset, (uint8_t)(g_offset >> 8), (uint8_t)(g_offset >> 16),
                       (uint8_t)(g_offset >> 24)};
    link_send(LINK_DATA, head, sizeof head, data, n);
    g_offset += n;
    data += n;
    len -= n;
  }
  return true;
}

void on_end(void *, bool complete) {
  uint8_t c = complete;
  link_send(LINK_END, nullptr, 0, &c, 1);
}

void on_done(void *, bool ok, const char *why) {
  uint8_t c = ok;
  link_send(LINK_DONE, &c, 1, why, why ? strlen(why) : 0);
}

void send_status(bool ready) {
  AwdlStatus a;
  awdl_status_read(&a);
  uint8_t body[2] = {(uint8_t)(ready ? 1 : 0), (uint8_t)(a.peers > 255 ? 255 : a.peers < 0 ? 0 : a.peers)};
  link_send(LINK_STATUS, nullptr, 0, body, sizeof body);
}

}  // namespace

extern "C" void app_main() {
  boot_hosted_next();
  link_init();
  const esp_app_desc_t *app = esp_app_get_description();
  link_send(LINK_HELLO, nullptr, 0, app->version, strlen(app->version));
  ESP_LOGI(TAG, "%s %s", app->project_name, app->version);
  log_boot_next();
  send_status(false);

  // On a failure below the receiver stops sending, status included: the P4
  // power-cycles the C6 after 5 s of silence, and it boots esp_hosted (made
  // the next boot above).
  esp_err_t err = awdl_begin();
  if (err != ESP_OK) {
    ESP_LOGE(TAG, "awdl_begin: %s", esp_err_to_name(err));
    return;
  }
  set_tx_rate();
  awdl_set_caps(kDpsFlags, 0);

  static const AdStreamSink sink = {on_begin, on_data, on_end, nullptr};
  ad_on_stream(&sink, on_done);
  int rc = ad_begin(kName, 0);
  if (rc != 0) {
    ESP_LOGE(TAG, "ad_begin: %d", rc);
    return;
  }
  ESP_LOGI(TAG, "ready as '%s'", kName);

  char line[200];
  TickType_t last_status = 0;
  for (;;) {
    ad_poll();
    while (ad_diag_read_line(line, sizeof line) > 0) {
      size_t n = strlen(line);
      while (n && line[n - 1] == '\n') n--;
      link_send(LINK_LOG, nullptr, 0, line, n);
    }
    if (xTaskGetTickCount() - last_status >= pdMS_TO_TICKS(1000)) {
      last_status = xTaskGetTickCount();
      send_status(true);
    }
    vTaskDelay(pdMS_TO_TICKS(10));
  }
}
