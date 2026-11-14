/* Wakey's addition to esp_hosted's slave: switching to the AirDrop receiver.
 *
 * The C6 has two app slots: esp_hosted runs from one, the AirDrop receiver
 * ("wakey_airdrop_c6", c6/airdrop) sits in the other. Over esp_hosted's peer
 * data channel the P4 can
 *   - ask what the other slot holds (WK_QUERY -> WK_INFO), and
 *   - make the receiver the next boot and restart into it (WK_BOOT_AIRDROP ->
 *     WK_RESULT, then the restart).
 * The receiver makes esp_hosted the next boot again first thing, so a power
 * cycle of the C6 always comes back here.
 */

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

#define WK_PROTOCOL 1
#define WK_RECEIVER "wakey_airdrop_c6"

static const char *TAG = "wakey";

/* WK_INFO payload: protocol, then the other slot's label, project and version
   (NUL-terminated, empty when the slot holds no app). */
struct __attribute__((packed)) wk_info {
  uint8_t protocol;
  char other_label[17];
  char other_project[32];
  char other_version[32];
};

static const esp_partition_t *other_slot(esp_app_desc_t *desc, bool *has_app) {
  const esp_partition_t *other = esp_ota_get_next_update_partition(NULL);
  *has_app = other && esp_ota_get_partition_description(other, desc) == ESP_OK;
  return other;
}

static void reply_result(esp_err_t err) {
  int32_t code = err;
  esp_hosted_send_custom_data(WK_RESULT, (const uint8_t *)&code, sizeof code);
}

static void restart_task(void *arg) {
  vTaskDelay(pdMS_TO_TICKS(200)); /* the reply goes out first */
  esp_restart();
}

static void on_message(uint32_t id, const uint8_t *data, size_t len, void *ctx) {
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
    esp_hosted_send_custom_data(WK_INFO, (const uint8_t *)&info, sizeof info);
    return;
  }
  /* WK_BOOT_AIRDROP */
  if (!has_app || strcmp(desc.project_name, WK_RECEIVER) != 0) {
    ESP_LOGW(TAG, "boot AirDrop refused: the other slot holds '%s'", has_app ? desc.project_name : "");
    reply_result(ESP_ERR_NOT_FOUND);
    return;
  }
  esp_err_t err = esp_ota_set_boot_partition(other);
  ESP_LOGI(TAG, "boot AirDrop %s %s from %s: %s", desc.project_name, desc.version, other->label,
           esp_err_to_name(err));
  reply_result(err);
  if (err == ESP_OK) xTaskCreate(restart_task, "wk_restart", 2048, NULL, 5, NULL);
}

void wakey_switch_init(void) {
  esp_hosted_register_custom_callback(WK_QUERY, on_message, NULL);
  esp_hosted_register_custom_callback(WK_BOOT_AIRDROP, on_message, NULL);
}
