/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * The receiver's link to the P4: frames on UART1, TX on IO2 (the P4's GPIO14).
 *
 * One way, no flow control: the P4 reads everything (it has the memory); a
 * full TX buffer blocks the writer, which for file data is ESP32Drop's TLS
 * task, so the sender is slowed down by TCP rather than data lost.
 *
 * Frame (little-endian):
 *   0xA5 0x5A, type (1 byte), length (2 bytes), payload (length bytes),
 *   CRC-32 (IEEE, as zlib's) over type, length and payload (4 bytes).
 * The payload is at most LINK_PAYLOAD_MAX bytes.
 *
 * Types and payloads (read by wakey-core/src/airdrop.rs, `FrameReader`, on
 * the P4; firmware/src/airdrop.rs runs it):
 *   HELLO   1  version string (the app's)
 *   STATUS  2  state (0 starting, 1 ready), AWDL peers (1 byte each)
 *   BEGIN   3  file size (4), name (rest, UTF-8)
 *   DATA    4  offset in the file (4), bytes (rest, at most LINK_DATA_MAX)
 *   END     5  1 if the file is complete
 *   DONE    6  1 if the transfer is whole and intact, then the reason (text)
 *   LOG     7  one log line (text)
 */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
  LINK_HELLO = 1,
  LINK_STATUS = 2,
  LINK_BEGIN = 3,
  LINK_DATA = 4,
  LINK_END = 5,
  LINK_DONE = 6,
  LINK_LOG = 7,
};

#define LINK_BAUD 3000000
/* The P4 takes a longer length for a false start and resynchronises:
   MAX_PAYLOAD in wakey-core/src/airdrop.rs, keep both equal. */
#define LINK_PAYLOAD_MAX 2048
/* File bytes per DATA frame (plus the 4-byte offset). */
#define LINK_DATA_MAX 1024

/* UART1 at LINK_BAUD on IO2; routes ESP_LOG into LOG frames. */
void link_init(void);

/* One frame: `head` (may be NULL) and `body` form the payload. Blocks while
   the TX buffer is full. False, and nothing sent, if the payload is longer
   than LINK_PAYLOAD_MAX. */
bool link_send(uint8_t type, const void *head, size_t head_len, const void *body, size_t body_len);

#ifdef __cplusplus
}
#endif
