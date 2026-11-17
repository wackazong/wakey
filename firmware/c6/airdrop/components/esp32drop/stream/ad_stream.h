/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * ad_stream.h -- decode an AirDrop /Upload body while it arrives, in bounded memory.
 *
 * ESP32Drop decodes a body into one block the size of the whole archive, which needs
 * PSRAM. The ESP32-C6 has none, and the files it receives (sound files, megabytes) are
 * passed on to the ESP32-P4, so this decoder never holds more than the 32 KiB inflate
 * window: body bytes go in as they arrive, file bytes come out through a sink.
 *
 * The body is a run of DvZip records (ad_dvzip.h: a 4-byte big-endian header, bit 31 =
 * stored, low 31 bits = payload length; a zero header ends the body), each payload one
 * zlib stream or literal bytes. Together they make one cpio archive (odc as Apple
 * senders write it, or newc), whose regular files are handed to the sink.
 *
 * Every byte comes from a remote device, so every length is bounded before it is used,
 * and so is the transfer as a whole (AD_STREAM_MAX_BYTES).
 * Plain C, no platform calls: the same file builds into the firmware (the C6's ROM
 * inflater) and into the host test (miniz 1.14, the same tinfl).
 */
#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Where decoded files go. Runs on the caller of ad_stream_feed(), in order:
 * begin, data..., end for each regular file (directories and other entries are skipped).
 * `name` is the basename, NUL-terminated, at most 255 bytes, valid only during the call.
 * It comes from the sender unchecked apart from that: it may be "." or ".." (from "./."
 * or "a/.."), so a sink that makes a path of it must reject those.
 *   begin: false skips this file's bytes (no data or end for it).
 *   data:  false stops the stream (AD_STREAM_E_SINK): the sink cannot take more.
 *   end:   `complete` is false if the stream stopped or was freed inside the file.
 * end(true) means all of the file's bytes were delivered, not that they were verified:
 * a record's adler32 is checked at its end, after its bytes went out, and damage can
 * also show first as a bad archive. So keep files provisional (a ".part") and commit
 * them only when the whole body succeeded: no error, and the body ended (end record,
 * or a record boundary when the sender finished). TLS below already authenticates every
 * byte, so this guards against bugs, not against the radio. */
struct AdStreamSink {
  bool (*begin)(void *ctx, const char *name, uint32_t size);
  bool (*data)(void *ctx, const uint8_t *data, size_t len);
  void (*end)(void *ctx, bool complete);
  void *ctx;
};

/* The largest transfer accepted: no entry may announce more, and the decoded archive
 * (the files, their headers and padding) may not be longer. The files are alarm sounds
 * (a song as FLAC is about 30 MB), and at this link's speed (about 100 kB/s) 256 MiB
 * already takes most of an hour. */
#define AD_STREAM_MAX_BYTES (256u * 1024 * 1024)

enum AdStreamError {
  AD_STREAM_OK = 0,
  AD_STREAM_E_RECORD,   /* a record header no sender writes (empty, or over 1 MiB)     */
  AD_STREAM_E_INFLATE,  /* a record's zlib stream is broken, short or followed by junk */
  AD_STREAM_E_ARCHIVE,  /* the cpio archive is malformed                               */
  AD_STREAM_E_SINK,     /* the sink refused data                                        */
  AD_STREAM_E_SIZE,     /* an entry or the whole archive is over AD_STREAM_MAX_BYTES   */
};

struct AdStreamStats {
  uint32_t records;     /* records completed                     */
  uint32_t stored;      /* ...of which stored                    */
  uint64_t in_bytes;    /* body bytes fed                        */
  uint64_t out_bytes;   /* archive bytes decoded                 */
  uint32_t files;       /* regular files completed (end(true))   */
};

typedef struct AdStream AdStream;

/* About 44 KB from the heap (inflate state and window). NULL if out of memory. */
AdStream *ad_stream_new(const struct AdStreamSink *sink);

/* Ends an open file with end(false), then frees. NULL is fine. */
void ad_stream_free(AdStream *s);

/* Decodes `len` more body bytes. False once the stream has failed; it then ignores
 * further input and ad_stream_error() says why. */
bool ad_stream_feed(AdStream *s, const uint8_t *data, size_t len);

/* Between records: the body may end here (senders often write no end record). */
bool ad_stream_at_record_boundary(const AdStream *s);
/* The zero record arrived: the body is complete. */
bool ad_stream_saw_end_record(const AdStream *s);
/* The archive's TRAILER!!! arrived: every file has been delivered. */
bool ad_stream_archive_done(const AdStream *s);

enum AdStreamError ad_stream_error(const AdStream *s);
const char *ad_stream_error_name(enum AdStreamError e);
void ad_stream_stats(const AdStream *s, struct AdStreamStats *out);

#ifdef __cplusplus
}
#endif
