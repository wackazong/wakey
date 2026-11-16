/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * Wakey's addition to esp_hosted's slave: switching to the AirDrop receiver.
 *
 * The C6 has two app slots: esp_hosted runs from one, the AirDrop receiver
 * ("wakey_airdrop_c6", c6/airdrop) sits in the other. Over esp_hosted's peer
 * data channel the P4 can
 *   - ask what the other slot holds and which esp_hosted runs
 *     (WK_QUERY -> WK_INFO), and
 *   - make the receiver the next boot and restart into it (WK_BOOT_AIRDROP ->
 *     WK_RESULT, then the restart).
 * The receiver makes esp_hosted the next boot again first thing, so a power
 * cycle of the C6 always comes back here.
 */

#include <inttypes.h>
#include <string.h>

#include "esp_app_desc.h"
#include "esp_hosted_peer_data.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

/* Message ids: 'W' 'K' and a number; replies have bit 15 set. */
#define WK_QUERY 0x574B0001u
#define WK_BOOT_AIRDROP 0x574B0002u
#define WK_INFO 0x574B8001u
#define WK_RESULT 0x574B8002u

/* 2: WK_INFO ends with this image's own version. */
#define WK_PROTOCOL 2
#define WK_RECEIVER "wakey_airdrop_c6"

/* Time for the reply to leave before the restart. */
#define WK_RESTART_DELAY_MS 200

static const char *TAG = "wakey";

/* WK_INFO payload: protocol, the other slot's label, project and version
   (NUL-terminated, empty when the slot holds no app), then this image's
   version (the P4 reinstalls esp_hosted when it differs from its copy). */
struct __attribute__((packed)) wk_info {
  uint8_t protocol;
  char other_label[17];
  char other_project[32];
  char other_version[32];
  char own_version[32];
};

static const esp_partition_t *other_slot(esp_app_desc_t *desc, bool *has_app) {
  const esp_partition_t *other = esp_ota_get_next_update_partition(NULL);
  *has_app = other && esp_ota_get_partition_description(other, desc) == ESP_OK;
  return other;
}

static void send(uint32_t id, const void *data, size_t len) {
  esp_err_t err = esp_hosted_send_custom_data(id, (const uint8_t *)data, len);
  if (err != ESP_OK) ESP_LOGW(TAG, "sending 0x%08" PRIx32 ": %s", id, esp_err_to_name(err));
}

static void reply_result(esp_err_t err) {
  int32_t code = err;
  send(WK_RESULT, &code, sizeof code);
}

static void restart_task(void *arg) {
  (void)arg;
  vTaskDelay(pdMS_TO_TICKS(WK_RESTART_DELAY_MS));
  esp_restart();
}

static void on_message(uint32_t id, const uint8_t *data, size_t len, void *ctx) {
  (void)data;
  (void)len;
  (void)ctx;
  esp_app_desc_t desc;
  bool has_app;
  const esp_partition_t *other = other_slot(&desc, &has_app);
  if (id == WK_QUERY) {
    struct wk_info info = {.protocol = WK_PROTOCOL};
    if (other) strlcpy(info.other_label, other->label, sizeof info.other_label);
    if (has_app) {
      strlcpy(info.other_project, desc.project_name, sizeof info.other_project);
      strlcpy(info.other_version, desc.version, sizeof info.other_version);
    }
    strlcpy(info.own_version, esp_app_get_description()->version, sizeof info.own_version);
    send(WK_INFO, &info, sizeof info);
  } else if (id == WK_BOOT_AIRDROP) {
    if (!has_app || strcmp(desc.project_name, WK_RECEIVER) != 0) {
      ESP_LOGW(TAG, "boot AirDrop refused: the other slot holds '%s'", has_app ? desc.project_name : "");
      reply_result(ESP_ERR_NOT_FOUND);
      return;
    }
    esp_err_t err = esp_ota_set_boot_partition(other);
    ESP_LOGI(TAG, "boot AirDrop %s %s from %s: %s", desc.project_name, desc.version, other->label,
             esp_err_to_name(err));
    reply_result(err);
    if (err != ESP_OK) return;
    /* The restart waits on its own task: this callback runs inside
       esp_hosted's RPC handling, which must not block. Without the task,
       wait here instead (the reply may then not leave, and the P4 reports
       the switch as failed, but the receiver starts). */
    if (xTaskCreate(restart_task, "wk_restart", 2048, NULL, 5, NULL) != pdPASS) {
      ESP_LOGW(TAG, "no restart task: restarting from here");
      vTaskDelay(pdMS_TO_TICKS(WK_RESTART_DELAY_MS));
      esp_restart();
    }
  }
}

void wakey_switch_init(void) {
  const uint32_t ids[] = {WK_QUERY, WK_BOOT_AIRDROP};
  for (size_t i = 0; i < sizeof ids / sizeof ids[0]; i++) {
    esp_err_t err = esp_hosted_register_custom_callback(ids[i], on_message, NULL);
    if (err != ESP_OK) ESP_LOGE(TAG, "registering 0x%08" PRIx32 ": %s", ids[i], esp_err_to_name(err));
  }
}
