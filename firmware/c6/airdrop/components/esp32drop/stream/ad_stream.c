/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * ad_stream.c -- see ad_stream.h.
 */
#include "ad_stream.h"

#include <stdlib.h>
#include <string.h>

/* tinfl: the C6's ROM inflater through ESP-IDF's esp_rom/include/miniz.h, or miniz 1.14
   in the host test (the same code and structure layout). */
#include "miniz.h"

/* The largest record payload accepted. Measured records are at most 131,118 bytes (a
   128 KiB grain that deflate made larger); ESP32Drop uses the same bound. */
#define REC_MAX (1u * 1024 * 1024)
#define REC_STORED 0x80000000u

/* cpio header sizes and the longest name kept (Apple senders write "./name"). */
#define ODC_HDR 76
#define NEWC_HDR 110
#define NAME_MAX_LEN 255

enum rec_state { R_HEADER, R_STORED, R_ZLIB, R_END };
enum cpio_state { C_HEADER, C_NAME, C_NAME_PAD, C_DATA, C_DATA_PAD, C_DONE };

struct AdStream {
  struct AdStreamSink sink;
  enum AdStreamError err;
  struct AdStreamStats stats;

  /* DvZip records */
  enum rec_state rs;
  uint8_t rhdr[4];
  uint32_t rhdr_n;    /* header bytes collected */
  uint32_t rec_left;  /* payload bytes still to come in this record */

  /* cpio */
  enum cpio_state cs;
  bool newc;
  uint8_t chdr[NEWC_HDR];
  uint32_t chdr_n;
  char name[NAME_MAX_LEN + 1];
  uint32_t name_n, name_len;  /* name_len includes the archive's NUL */
  uint32_t file_size, file_left, pad_left;
  bool regular;       /* the current entry is a regular file */
  bool file_open;     /* ...and begin() accepted it */

  /* inflate */
  size_t win_pos;
  tinfl_decompressor inf;
  uint8_t win[TINFL_LZ_DICT_SIZE];
};

static bool fail(AdStream *s, enum AdStreamError e) {
  if (s->err == AD_STREAM_OK) s->err = e;
  if (s->file_open) {
    s->file_open = false;
    if (s->sink.end) s->sink.end(s->sink.ctx, false);
  }
  return false;
}

static uint32_t min_u32(uint32_t a, size_t b) { return b < a ? (uint32_t)b : a; }

/* n ASCII digits in base 8 or 16; false on anything else (then it is not a header) and
   on a value above UINT32_MAX (odc's 11 octal digits hold 33 bits). */
static bool parse_num(const uint8_t *p, int n, int base, uint32_t *out) {
  uint32_t v = 0;
  for (int i = 0; i < n; i++) {
    uint8_t c = p[i];
    uint32_t d;
    if (c >= '0' && c <= '9') d = (uint32_t)(c - '0');
    else if (c >= 'a' && c <= 'f') d = (uint32_t)(c - 'a' + 10);
    else if (c >= 'A' && c <= 'F') d = (uint32_t)(c - 'A' + 10);
    else return false;
    if (d >= (uint32_t)base) return false;
    if (v > (UINT32_MAX - d) / (uint32_t)base) return false;
    v = v * (uint32_t)base + d;
  }
  *out = v;
  return true;
}

static uint32_t pad4(uint32_t n) { return (4 - (n & 3)) & 3; }

/* A header is complete: read mode, name size and file size. */
static bool cpio_header(AdStream *s) {
  uint32_t mode, namesize, filesize;
  bool ok = s->newc ? parse_num(s->chdr + 14, 8, 16, &mode) &&
                          parse_num(s->chdr + 54, 8, 16, &filesize) &&
                          parse_num(s->chdr + 94, 8, 16, &namesize)
                    : parse_num(s->chdr + 18, 6, 8, &mode) &&
                          parse_num(s->chdr + 59, 6, 8, &namesize) &&
                          parse_num(s->chdr + 65, 11, 8, &filesize);
  if (!ok || namesize == 0 || namesize > NAME_MAX_LEN + 1) return fail(s, AD_STREAM_E_ARCHIVE);
  s->name_len = namesize;
  s->name_n = 0;
  s->file_size = filesize;
  /* mode 0: the archive did not say; deliver it rather than lose it (as ad_cpio.h). */
  s->regular = mode == 0 || (mode & 0170000u) == 0100000u;
  s->cs = C_NAME;
  return true;
}

/* State changes that need no more bytes happen at once, so a file ends (and its end()
   runs) with its last byte, not with the next entry's first. */
static void cpio_next_entry(AdStream *s) {
  s->cs = C_HEADER;
  s->chdr_n = 0;
}

static void cpio_file_done(AdStream *s) {
  if (s->file_open) {
    s->file_open = false;
    s->stats.files++;
    if (s->sink.end) s->sink.end(s->sink.ctx, true);
  }
  s->pad_left = s->newc ? pad4(s->file_size) : 0;
  if (s->pad_left > 0) s->cs = C_DATA_PAD;
  else cpio_next_entry(s);
}

static void cpio_data_start(AdStream *s) {
  s->cs = C_DATA;
  if (s->file_left == 0) cpio_file_done(s);
}

/* The name is complete: the trailer, or a file to announce. */
static bool cpio_name(AdStream *s) {
  if (s->name[s->name_len - 1] != '\0') return fail(s, AD_STREAM_E_ARCHIVE);
  if (strcmp(s->name, "TRAILER!!!") == 0) {
    s->cs = C_DONE;
    return true;
  }
  const char *base = strrchr(s->name, '/');
  base = base ? base + 1 : s->name;
  s->file_open = false;
  if (s->regular && base[0] != '\0' && s->sink.begin) {
    s->file_open = s->sink.begin(s->sink.ctx, base, s->file_size);
  }
  s->file_left = s->file_size;
  s->pad_left = s->newc ? pad4(NEWC_HDR + s->name_len) : 0;
  if (s->pad_left > 0) s->cs = C_NAME_PAD;
  else cpio_data_start(s);
  return true;
}

/* Archive bytes, as they come out of the records. */
static bool cpio_feed(AdStream *s, const uint8_t *p, size_t n) {
  s->stats.out_bytes += n;
  while (n > 0) {
    switch (s->cs) {
      case C_HEADER: {
        if (s->chdr_n < 6) {
          uint32_t take = min_u32(6 - s->chdr_n, n);
          memcpy(s->chdr + s->chdr_n, p, take);
          s->chdr_n += take, p += take, n -= take;
          if (s->chdr_n < 6) break;
          if (memcmp(s->chdr, "070707", 6) == 0) s->newc = false;
          else if (memcmp(s->chdr, "070701", 6) == 0 || memcmp(s->chdr, "070702", 6) == 0)
            s->newc = true;
          else return fail(s, AD_STREAM_E_ARCHIVE);
        }
        uint32_t len = s->newc ? NEWC_HDR : ODC_HDR;
        uint32_t take = min_u32(len - s->chdr_n, n);
        memcpy(s->chdr + s->chdr_n, p, take);
        s->chdr_n += take, p += take, n -= take;
        if (s->chdr_n == len && !cpio_header(s)) return false;
        break;
      }
      case C_NAME: {
        uint32_t take = min_u32(s->name_len - s->name_n, n);
        memcpy(s->name + s->name_n, p, take);
        s->name_n += take, p += take, n -= take;
        if (s->name_n == s->name_len && !cpio_name(s)) return false;
        break;
      }
      case C_NAME_PAD:
      case C_DATA_PAD: {
        uint32_t take = min_u32(s->pad_left, n);
        s->pad_left -= take, p += take, n -= take;
        if (s->pad_left > 0) break;
        if (s->cs == C_NAME_PAD) cpio_data_start(s);
        else cpio_next_entry(s);
        break;
      }
      case C_DATA: {
        uint32_t take = min_u32(s->file_left, n);
        if (s->file_open && s->sink.data && !s->sink.data(s->sink.ctx, p, take))
          return fail(s, AD_STREAM_E_SINK);
        s->file_left -= take, p += take, n -= take;
        if (s->file_left == 0) cpio_file_done(s);
        break;
      }
      case C_DONE:
        return true; /* padding after the trailer */
    }
  }
  return true;
}

/* Feeds zlib payload bytes of the current record through the window. */
static bool inflate_feed(AdStream *s, const uint8_t **pp, size_t *pn) {
  for (;;) {
    size_t in = min_u32(s->rec_left, *pn);
    size_t out = TINFL_LZ_DICT_SIZE - s->win_pos;
    mz_uint32 flags = TINFL_FLAG_PARSE_ZLIB_HEADER;
    if (s->rec_left > in) flags |= TINFL_FLAG_HAS_MORE_INPUT;
    tinfl_status st = tinfl_decompress(&s->inf, *pp, &in, s->win, s->win + s->win_pos, &out, flags);
    *pp += in, *pn -= in, s->rec_left -= (uint32_t)in;
    if (out > 0) {
      if (!cpio_feed(s, s->win + s->win_pos, out)) return false;
      s->win_pos = (s->win_pos + out) & (TINFL_LZ_DICT_SIZE - 1);
    }
    if (st < TINFL_STATUS_DONE) return fail(s, AD_STREAM_E_INFLATE);
    if (st == TINFL_STATUS_DONE) {
      if (s->rec_left > 0) return fail(s, AD_STREAM_E_INFLATE); /* bytes after the stream */
      s->stats.records++;
      s->rs = R_HEADER;
      return true;
    }
    if (st == TINFL_STATUS_HAS_MORE_OUTPUT) continue;
    /* NEEDS_MORE_INPUT */
    if (s->rec_left == 0) return fail(s, AD_STREAM_E_INFLATE); /* the record ended first */
    if (*pn == 0) return true;
  }
}

AdStream *ad_stream_new(const struct AdStreamSink *sink) {
  AdStream *s = (AdStream *)calloc(1, sizeof *s);
  if (s) s->sink = *sink;
  return s;
}

void ad_stream_free(AdStream *s) {
  if (!s) return;
  if (s->file_open && s->sink.end) s->sink.end(s->sink.ctx, false);
  free(s);
}

bool ad_stream_feed(AdStream *s, const uint8_t *p, size_t n) {
  if (s->err != AD_STREAM_OK) return false;
  s->stats.in_bytes += n;
  while (n > 0) {
    switch (s->rs) {
      case R_HEADER: {
        uint32_t take = min_u32(4 - s->rhdr_n, n);
        memcpy(s->rhdr + s->rhdr_n, p, take);
        s->rhdr_n += take, p += take, n -= take;
        if (s->rhdr_n < 4) break;
        s->rhdr_n = 0;
        uint32_t hdr = (uint32_t)s->rhdr[0] << 24 | (uint32_t)s->rhdr[1] << 16 |
                       (uint32_t)s->rhdr[2] << 8 | s->rhdr[3];
        s->rec_left = hdr & ~REC_STORED;
        if (hdr == 0) {
          s->rs = R_END;
        } else if (s->rec_left == 0 || s->rec_left > REC_MAX) {
          return fail(s, AD_STREAM_E_RECORD);
        } else if (hdr & REC_STORED) {
          s->rs = R_STORED;
        } else {
          tinfl_init(&s->inf);
          s->rs = R_ZLIB;
        }
        break;
      }
      case R_STORED: {
        uint32_t take = min_u32(s->rec_left, n);
        if (!cpio_feed(s, p, take)) return false;
        s->rec_left -= take, p += take, n -= take;
        if (s->rec_left == 0) {
          s->stats.records++;
          s->stats.stored++;
          s->rs = R_HEADER;
        }
        break;
      }
      case R_ZLIB:
        if (!inflate_feed(s, &p, &n)) return false;
        break;
      case R_END:
        return true; /* nothing belongs after the end record; ignore it */
    }
  }
  return true;
}

bool ad_stream_at_record_boundary(const AdStream *s) {
  return s->err == AD_STREAM_OK && s->rs == R_HEADER && s->rhdr_n == 0;
}

bool ad_stream_saw_end_record(const AdStream *s) { return s->rs == R_END; }

bool ad_stream_archive_done(const AdStream *s) { return s->cs == C_DONE; }

enum AdStreamError ad_stream_error(const AdStream *s) { return s->err; }

const char *ad_stream_error_name(enum AdStreamError e) {
  switch (e) {
    case AD_STREAM_OK: return "ok";
    case AD_STREAM_E_RECORD: return "bad record header";
    case AD_STREAM_E_INFLATE: return "record would not inflate";
    case AD_STREAM_E_ARCHIVE: return "bad cpio archive";
    case AD_STREAM_E_SINK: return "sink refused data";
  }
  return "?";
}

void ad_stream_stats(const AdStream *s, struct AdStreamStats *out) { *out = s->stats; }
