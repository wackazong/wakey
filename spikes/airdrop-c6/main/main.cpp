// Spike: can the Tab5's ESP32-C6 receive AirDrop from an iPhone?
//
// Runs ESP32Drop's receiver (AWDL on 2.4 GHz channel 6 + the AirDrop TLS
// server) instead of esp_hosted, with uploads decoded while they arrive
// (components/esp32drop/stream): the C6 has no PSRAM, and on the clock the
// files go on to the P4. Here they are only counted and checksummed.
//
// The P4 installs this image through esp_hosted's OTA and prints the log,
// which leaves the C6 on IO2 (see README.md). First thing at start the boot
// slot goes back to esp_hosted, so any reset or power cycle of the C6 brings
// the normal firmware back: the C6 has no rollback and no cable to recover it.

#include <cinttypes>
#include <cstdio>
#include <cstring>

#include "ESP32Drop.h"
#include "driver/uart.h"
#include "esp_app_desc.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_ota_ops.h"
#include "esp_rom_crc.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "mbedtls/ctr_drbg.h"
#include "mbedtls/ecdh.h"
#include "mbedtls/ecdsa.h"
#include "mbedtls/entropy.h"

// Spike diagnostics in the vendor tree (patches/0004, 0006).
extern "C" void awdl_tx_census_read(uint32_t out[8]);
extern "C" void awdl_tcp_census_read(uint32_t out[5]);
extern "C" void awdl_rx_ucast_read(uint32_t *n, uint32_t *retry);
extern "C" int awdl_tx_last_rate(void);
extern "C" uint32_t awdl_netif_inflight_reaped(void);
extern "C" void awdl_rx_format_read(uint32_t out[8], uint8_t *last_rate);
extern "C" size_t awdl_peers_describe(char *out, size_t cap, const uint8_t master[6]);
extern "C" uint32_t awdl_follow_tx_read(void);

namespace {

const char *const TAG = "airdrop";

// The name on the sender's share sheet.
constexpr const char *kName = "Wakey";
// The esp_hosted slave's project name (firmware/c6/).
constexpr const char *kHostedProject = "network_adapter";

int64_t now_ms() { return esp_timer_get_time() / 1000; }

// Makes the other OTA slot, if it holds esp_hosted, the one the C6 boots next.
// Kept out of app_main's frame (noinline): it runs first, with the stack all its own.
__attribute__((noinline)) void boot_hosted_next() {
  const esp_partition_t *running = esp_ota_get_running_partition();
  const esp_partition_t *other = esp_ota_get_next_update_partition(nullptr);
  esp_app_desc_t desc;
  if (other == nullptr || esp_ota_get_partition_description(other, &desc) != ESP_OK) {
    ESP_LOGE(TAG, "no app in the other slot: the C6 keeps booting this spike");
    return;
  }
  if (strcmp(desc.project_name, kHostedProject) != 0) {
    ESP_LOGE(TAG, "the other slot holds '%s', not esp_hosted: boot slot unchanged",
             desc.project_name);
    return;
  }
  esp_err_t err = esp_ota_set_boot_partition(other);
  ESP_LOGI(TAG, "running from %s; next boot: %s %s from %s (%s)", running->label,
           desc.project_name, desc.version, other->label, esp_err_to_name(err));
}

// ---- capabilities we announce ----

// ESP32Drop announces dualband support (data_path_state flag 0x0020). Saying "2.4 GHz
// only" made the same iPhone schedule (3 of 16 slots on channel 6) deliver 123-136
// kB/s instead of 79-98 kB/s. Short GI for HT20 on top (ht_capabilities 0x11ee) was
// received fine but measured no faster (128-129 kB/s): not announced.
constexpr uint16_t kDpsFlags = 0x8f24 & ~0x0020;  // 0x8f04

// ---- the raw transmit rate ----

// AWDL's raw frames (action frames, our TCP ACKs) go out at 1 Mbit/s by default,
// ~1 ms of a channel-6 slot per ACK. 11g 24 and 54 Mbit/s measured the same
// (23.4 kB/s each, one ch6 slot per cycle); 24 keeps more margin at range.
// HT20 (MCS4) wedges the C6's transmitter: every frame after the switch failed
// with "(tx)rts error ... 0xab".
void set_tx_rate() {
  wifi_tx_rate_config_t config = {WIFI_PHY_MODE_11G, WIFI_PHY_RATE_24M, false, false};
  esp_err_t err = esp_wifi_config_80211_tx(WIFI_IF_STA, &config);
  ESP_LOGI(TAG, "raw TX rate: 11g 24 Mbit/s (%s)", esp_err_to_name(err));
}

// ---- the stream sink: runs on ESP32Drop's TLS task, so it only counts ----

struct Receipt {
  char name[64];
  uint32_t size;
  uint32_t got;
  uint32_t crc;
  int64_t start_ms;
} g_file;

int64_t g_transfer_start_ms;
uint64_t g_transfer_bytes;

bool on_begin(void *, const char *name, uint32_t size) {
  if (g_transfer_start_ms == 0) g_transfer_start_ms = now_ms();
  strlcpy(g_file.name, name, sizeof g_file.name);
  g_file.size = size;
  g_file.got = 0;
  g_file.crc = 0;
  g_file.start_ms = now_ms();
  ESP_LOGI(TAG, "file '%s', %" PRIu32 " B: receiving", name, size);
  return true;
}

bool on_data(void *, const uint8_t *data, size_t len) {
  g_file.crc = esp_rom_crc32_le(g_file.crc, data, len);
  g_file.got += len;
  g_transfer_bytes += len;
  return true;
}

void on_end(void *, bool complete) {
  ESP_LOGI(TAG, "file '%s': %s, %" PRIu32 " of %" PRIu32 " B, crc32 %08" PRIx32 ", %" PRId64 " ms",
           g_file.name, complete ? "complete" : "INCOMPLETE", g_file.got, g_file.size, g_file.crc,
           now_ms() - g_file.start_ms);
}

void on_done(void *, bool ok, const char *why) {
  int64_t ms = g_transfer_start_ms ? now_ms() - g_transfer_start_ms : 0;
  ESP_LOGI(TAG, "transfer %s (%s): %" PRIu64 " B of files in %" PRId64 " ms = %.1f kB/s",
           ok ? "OK" : "FAILED", why, g_transfer_bytes, ms,
           ms > 0 ? g_transfer_bytes / (double)ms : 0.0);
  g_transfer_start_ms = 0;
  g_transfer_bytes = 0;
}

// ---- the handshake's cost ----

// The P-256 work of a TLS server handshake (ECDHE-ECDSA): the ephemeral key, the
// signature over it, the shared secret. ESP32Drop needs the server's flight inside
// one ~52 ms channel-6 window (ad_port_esp32.cpp, TLS config).
void bench_handshake_crypto() {
  mbedtls_entropy_context entropy;
  mbedtls_ctr_drbg_context rng;
  mbedtls_ecdsa_context key;
  mbedtls_ecp_group grp;
  mbedtls_mpi d, z;
  mbedtls_ecp_point q;
  mbedtls_entropy_init(&entropy);
  mbedtls_ctr_drbg_init(&rng);
  mbedtls_ecdsa_init(&key);
  mbedtls_ecp_group_init(&grp);
  mbedtls_mpi_init(&d);
  mbedtls_mpi_init(&z);
  mbedtls_ecp_point_init(&q);
  mbedtls_ctr_drbg_seed(&rng, mbedtls_entropy_func, &entropy, nullptr, 0);
  mbedtls_ecp_group_load(&grp, MBEDTLS_ECP_DP_SECP256R1);
  mbedtls_ecdsa_genkey(&key, MBEDTLS_ECP_DP_SECP256R1, mbedtls_ctr_drbg_random, &rng);

  unsigned char hash[32] = {1}, sig[MBEDTLS_ECDSA_MAX_LEN];
  size_t sig_len = 0;
  int64_t t0 = esp_timer_get_time();
  int e1 = mbedtls_ecdh_gen_public(&grp, &d, &q, mbedtls_ctr_drbg_random, &rng);
  int64_t t1 = esp_timer_get_time();
  int e2 = mbedtls_ecdsa_write_signature(&key, MBEDTLS_MD_SHA256, hash, sizeof hash, sig,
                                         sizeof sig, &sig_len, mbedtls_ctr_drbg_random, &rng);
  int64_t t2 = esp_timer_get_time();
  int e3 = mbedtls_ecdh_compute_shared(&grp, &z, &q, &d, mbedtls_ctr_drbg_random, &rng);
  int64_t t3 = esp_timer_get_time();
  ESP_LOGI(TAG, "P-256: ephemeral key %lld ms, ECDSA sign %lld ms, shared secret %lld ms (%d %d %d)",
           (t1 - t0) / 1000, (t2 - t1) / 1000, (t3 - t2) / 1000, e1, e2, e3);

  mbedtls_ecp_point_free(&q);
  mbedtls_mpi_free(&z);
  mbedtls_mpi_free(&d);
  mbedtls_ecp_group_free(&grp);
  mbedtls_ecdsa_free(&key);
  mbedtls_ctr_drbg_free(&rng);
  mbedtls_entropy_free(&entropy);
}

// ---- status ----

void log_heap(const char *when) {
  ESP_LOGI(TAG, "heap %s: %u free, %u min, %u largest", when,
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
           (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL),
           (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL));
}

// One line: the link, the AirDrop funnel, the heap. On change (at most once a
// second), every second while receiving, and every 30 s regardless.
void log_status() {
  static char last[192];
  static int64_t last_ms = 0;
  AwdlStatus a;
  AdStatus d;
  awdl_status_read(&a);
  ad_stats_read(&d);

  // The funnel and the link state; the window counters move all the time.
  char key[192];
  snprintf(key, sizeof key,
           "%s%s peers %" PRId32 " | mdns %" PRIu32 "/%" PRIu32 " tcp %" PRIu32 " tls %" PRIu32
           " discover %" PRIu32 " ask %" PRIu32 " upload %" PRIu32 " files %" PRIu32
           " err %" PRIu32 " %s",
           a.have_master ? "master" : "no master", a.locked ? " locked" : "", a.peers, d.resp_tx,
           d.query_seen, d.tcp_accept, d.tls_ok, d.discover_ok, d.ask_ok, d.upload_ok, d.files_ok,
           d.file_err, d.file_errmsg[0] ? d.file_errmsg : "-");
  int64_t t = now_ms();
  bool due = (strcmp(key, last) != 0 && t - last_ms >= 1000) || t - last_ms >= 30000 ||
             (d.rx_active && t - last_ms >= 1000);
  if (!due) return;
  ESP_LOGI(TAG, "link %s; win %" PRIu32 "/%" PRIu32 " txerr %" PRIu32 "%s; heap %u (min %u)", key,
           a.win_served, a.win_total, a.tx_errors, d.rx_active ? "; RECEIVING" : "",
           (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
           (unsigned)heap_caps_get_minimum_free_size(MALLOC_CAP_INTERNAL));
  if (d.rx_active) ESP_LOGI(TAG, "  %" PRIu32 " B on the wire", d.rx_bytes);
  // The IPv6 link: what lwIP handed down, what the radio accepted, why not.
  AwdlDiag g;
  awdl_diag_read(&g, false);
  ESP_LOGI(TAG,
           "  netif rx %" PRIu32 " tx %" PRIu32 " sent %" PRIu32 " err %" PRIu32
           " (last %" PRId32 ", len %u, ucast %" PRIu32 " mcast %" PRIu32 ", nh %u)"
           " qdrop %" PRIu32 " retry %" PRIu32 "/%" PRIu32 "/%" PRIu32 " inflight max %u"
           " qmax %u reaped %" PRIu32 "; mif %" PRIu32 " err %" PRIu32,
           g.netif_rx, g.netif_tx, g.netif_tx_sent, g.netif_tx_err, g.netif_tx_last_err,
           (unsigned)g.netif_tx_err_len, g.netif_tx_err_ucast, g.netif_tx_err_mcast,
           (unsigned)g.netif_tx_err_nh, g.netif_txq_drop, g.netif_retry_queued,
           g.netif_retry_drop, g.netif_retry_exhausted, (unsigned)g.netif_inflight_max,
           (unsigned)g.txq_depth_max, awdl_netif_inflight_reaped(), g.mif_tx, g.mif_tx_err);
  uint32_t c[8];
  awdl_tx_census_read(c);
  ESP_LOGI(TAG, "  tx: tcp %" PRIu32 " mdns %" PRIu32 " udp %" PRIu32 " ns %" PRIu32 " na %" PRIu32
           " mld %" PRIu32 " icmp %" PRIu32 " other %" PRIu32, c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]);
  uint32_t k[5];
  awdl_tcp_census_read(k);
  ESP_LOGI(TAG, "  tcp: rst %" PRIu32 " syn %" PRIu32 " fin %" PRIu32 " data %" PRIu32 " ack %" PRIu32,
           k[0], k[1], k[2], k[3], k[4]);
  uint32_t fm[8];
  uint8_t lsig;
  awdl_rx_format_read(fm, &lsig);
  ESP_LOGI(TAG, "  rx for us by format: 11b %" PRIu32 " 11g %" PRIu32 " HT %" PRIu32 " VHT %" PRIu32
           " HE %" PRIu32 " (L-SIG rate %u); ch6 slots %d of 16 (mask %04x); ring drops %" PRIu32,
           fm[0], fm[1], fm[2], fm[3], fm[4] + fm[5] + fm[6] + fm[7], (unsigned)lsig,
           __builtin_popcount(a.ch6_mask), (unsigned)a.ch6_mask, g.ring_drop);
  uint32_t un, ur;
  awdl_rx_ucast_read(&un, &ur);
  ESP_LOGI(TAG, "  rx unicast data for us %" PRIu32 ", as 802.11 retries %" PRIu32
           "; tx unicast acked %" PRIu32 " not %" PRIu32 " (rate %d), action ok %" PRIu32 " fail %" PRIu32,
           un, ur, g.txdone_ucast_ok, g.txdone_ucast_fail, awdl_tx_last_rate(), g.txdone_action_ok,
           g.txdone_action_fail);
  char peers[320];
  awdl_peers_describe(peers, sizeof peers, a.master);
  ESP_LOGI(TAG, "  peers (* master, > sender):%s; sent outside the window to the sender %" PRIu32,
           peers, awdl_follow_tx_read());
  strlcpy(last, key, sizeof last);
  last_ms = t;
}

}  // namespace

extern "C" void app_main() {
  // The bootloader routes the console's pins; the app only sets the baud
  // rate. The C6 keeps esp_hosted's bootloader (default pins, the J1 header),
  // so route TX to IO2 here; the startup's own lines are lost.
  uart_set_pin(static_cast<uart_port_t>(CONFIG_ESP_CONSOLE_UART_NUM),
               CONFIG_ESP_CONSOLE_UART_TX_GPIO, UART_PIN_NO_CHANGE, UART_PIN_NO_CHANGE,
               UART_PIN_NO_CHANGE);
  const esp_app_desc_t *app = esp_app_get_description();
  ESP_LOGI(TAG, "%s %s (IDF %s)", app->project_name, app->version, app->idf_ver);
  boot_hosted_next();
  log_heap("at start");
  bench_handshake_crypto();

  esp_err_t err = awdl_begin();
  if (err != ESP_OK) {
    ESP_LOGE(TAG, "awdl_begin: %s", esp_err_to_name(err));
    return;
  }
  log_heap("with AWDL");

  const AdStreamSink sink = {on_begin, on_data, on_end, nullptr};
  ad_on_stream(&sink, on_done);
  int rc = ad_begin(kName, 0);
  if (rc != 0) {
    ESP_LOGE(TAG, "ad_begin: %d", rc);
    return;
  }
  log_heap("with the AirDrop listener");
  ESP_LOGI(TAG, "ready: '%s' in the AirDrop list (receiving set to Everyone)", kName);

  char line[200];
  set_tx_rate();
  awdl_set_caps(kDpsFlags, 0);
  for (;;) {
    ad_poll();
    while (ad_diag_read_line(line, sizeof line) > 0) printf("ad: %s", line);
    log_status();
    vTaskDelay(pdMS_TO_TICKS(10));
  }
}
