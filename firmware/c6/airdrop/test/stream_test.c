/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * Host test of ad_stream: archives built here, encoded as an iOS/macOS sender does
 * (128 KiB grains, each deflated with a zlib header or stored with bit 31 set; with or
 * without the zero end record), then fed in pieces of many sizes. The real body this
 * shape is taken from: ESP32Drop's testdata/dvzip_census.inc (12 deflated records of
 * 128 KiB, 6 stored, no end record). */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "ad_stream.h"
#include "miniz.h"

static int g_failed;

#define CHECK(cond, ...)                                       \
  do {                                                         \
    if (!(cond)) {                                             \
      g_failed++;                                              \
      fprintf(stderr, "%s:%d: %s: ", __FILE__, __LINE__, #cond); \
      fprintf(stderr, __VA_ARGS__);                            \
      fputc('\n', stderr);                                     \
    }                                                          \
  } while (0)

/* ---- a growable byte buffer ---------------------------------------------------- */

struct buf {
  uint8_t *p;
  size_t len, cap;
};

static void buf_put(struct buf *b, const void *data, size_t n) {
  if (b->len + n > b->cap) {
    b->cap = (b->len + n) * 2 + 64;
    b->p = realloc(b->p, b->cap);
  }
  if (n) memcpy(b->p + b->len, data, n);
  b->len += n;
}

static void buf_printf(struct buf *b, const char *fmt, ...) {
  char tmp[256];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(tmp, sizeof tmp, fmt, ap);
  va_end(ap);
  buf_put(b, tmp, (size_t)n);
}

static void buf_zeros(struct buf *b, size_t n) {
  static const uint8_t z[4];
  buf_put(b, z, n);
}

/* ---- archives -------------------------------------------------------------------- */

#define MODE_FILE 0100644
#define MODE_DIR 0040755

static void odc_add(struct buf *a, const char *name, unsigned mode, const uint8_t *data,
                    size_t len) {
  size_t ns = strlen(name) + 1;
  buf_printf(a, "070707%06o%06o%06o%06o%06o%06o%06o%011o%06o%011o", 0, 1, mode, 501, 20, 1, 0,
             0, (unsigned)ns, (unsigned)len);
  buf_put(a, name, ns);
  buf_put(a, data, len);
}

static void newc_add(struct buf *a, const char *name, unsigned mode, const uint8_t *data,
                     size_t len) {
  size_t ns = strlen(name) + 1;
  buf_printf(a, "070701%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x%08x", 1, mode, 0, 0, 1,
             0, (unsigned)len, 0, 0, 0, 0, (unsigned)ns, 0);
  buf_put(a, name, ns);
  buf_zeros(a, (4 - ((110 + ns) & 3)) & 3);
  buf_put(a, data, len);
  buf_zeros(a, (4 - (len & 3)) & 3);
}

/* ---- DvZip -------------------------------------------------------------------- */

#define GRAIN (128u * 1024)

enum codec { SENDER, ALWAYS_DEFLATE, ALWAYS_STORE };

static void put_be32(struct buf *b, uint32_t v) {
  uint8_t h[4] = {(uint8_t)(v >> 24), (uint8_t)(v >> 16), (uint8_t)(v >> 8), (uint8_t)v};
  buf_put(b, h, 4);
}

/* SENDER: deflate unless that does not pay, as a real sender does. */
static struct buf dvzip(const struct buf *archive, enum codec codec, bool end_record) {
  struct buf body = {0};
  for (size_t off = 0; off < archive->len; off += GRAIN) {
    size_t n = archive->len - off < GRAIN ? archive->len - off : GRAIN;
    size_t zlen = 0;
    void *z = NULL;
    if (codec != ALWAYS_STORE)
      z = tdefl_compress_mem_to_heap(archive->p + off, n, &zlen,
                                     TDEFL_WRITE_ZLIB_HEADER | TDEFL_DEFAULT_MAX_PROBES);
    if (z && (codec == ALWAYS_DEFLATE || zlen < n)) {
      put_be32(&body, (uint32_t)zlen);
      buf_put(&body, z, zlen);
    } else {
      put_be32(&body, 0x80000000u | (uint32_t)n);
      buf_put(&body, archive->p + off, n);
    }
    free(z);
  }
  if (end_record) put_be32(&body, 0);
  return body;
}

/* ---- the sink: keeps what arrives -------------------------------------------- */

#define MAX_FILES 16

struct got {
  int n;
  char name[MAX_FILES][300];
  struct buf data[MAX_FILES];
  uint32_t size[MAX_FILES];
  int ended[MAX_FILES]; /* 0 open, 1 complete, -1 incomplete */
  const char *refuse_name; /* begin() refuses this one */
  size_t refuse_after;     /* data() refuses past this many bytes (0: never) */
  size_t total;
};

static bool sink_begin(void *ctx, const char *name, uint32_t size) {
  struct got *g = ctx;
  if (g->refuse_name && strcmp(name, g->refuse_name) == 0) return false;
  if (g->n == MAX_FILES) return false;
  CHECK(g->n == 0 || g->ended[g->n - 1] != 0, "begin while a file is open");
  snprintf(g->name[g->n], sizeof g->name[0], "%s", name);
  g->size[g->n] = size;
  g->n++;
  return true;
}

static bool sink_data(void *ctx, const uint8_t *data, size_t len) {
  struct got *g = ctx;
  CHECK(g->n > 0 && g->ended[g->n - 1] == 0, "data outside a file");
  CHECK(len > 0, "empty data call");
  if (g->refuse_after && g->total + len > g->refuse_after) return false;
  g->total += len;
  buf_put(&g->data[g->n - 1], data, len);
  return true;
}

static void sink_end(void *ctx, bool complete) {
  struct got *g = ctx;
  CHECK(g->n > 0 && g->ended[g->n - 1] == 0, "end without an open file");
  g->ended[g->n - 1] = complete ? 1 : -1;
}

static void got_free(struct got *g) {
  for (int i = 0; i < MAX_FILES; i++) free(g->data[i].p);
}

/* ---- feeding --------------------------------------------------------------------- */

static uint32_t g_rng = 12345;
static uint32_t rnd(void) { return g_rng = g_rng * 1103515245u + 12345u; }

/* piece == 0: random sizes 1..20000. */
static AdStream *feed(const struct buf *body, size_t piece, struct got *g, bool *ok) {
  struct AdStreamSink sink = {sink_begin, sink_data, sink_end, g};
  AdStream *s = ad_stream_new(&sink);
  *ok = true;
  for (size_t off = 0; off < body->len;) {
    size_t n = piece ? piece : 1 + rnd() % 20000;
    if (n > body->len - off) n = body->len - off;
    if (!ad_stream_feed(s, body->p + off, n)) {
      *ok = false;
      break;
    }
    off += n;
  }
  return s;
}

/* ---- test data ------------------------------------------------------------------ */

struct file {
  const char *path;  /* as in the archive */
  const char *base;  /* as delivered */
  unsigned mode;
  struct buf data;
};

static struct buf random_bytes(size_t n) {
  struct buf b = {0};
  for (size_t i = 0; i < n; i++) {
    uint8_t c = (uint8_t)(rnd() >> 16);
    buf_put(&b, &c, 1);
  }
  return b;
}

static struct buf text_bytes(size_t n) {
  struct buf b = {0};
  while (b.len < n) buf_printf(&b, "Guten Morgen, es ist %zu Uhr. ", b.len % 24);
  b.len = n;
  return b;
}

static struct buf archive_of(struct file *f, int n, bool newc) {
  struct buf a = {0};
  for (int i = 0; i < n; i++) {
    if (newc) newc_add(&a, f[i].path, f[i].mode, f[i].data.p, f[i].data.len);
    else odc_add(&a, f[i].path, f[i].mode, f[i].data.p, f[i].data.len);
  }
  if (newc) newc_add(&a, "TRAILER!!!", 0, NULL, 0);
  else odc_add(&a, "TRAILER!!!", 0, NULL, 0);
  buf_zeros(&a, 3); /* cpio pads the end; nothing may read it as an entry */
  return a;
}

/* Every regular file of f arrived complete and byte for byte. */
static void check_files(const struct got *g, struct file *f, int n, const char *label) {
  int k = 0;
  for (int i = 0; i < n; i++) {
    if (f[i].mode == MODE_DIR) continue;
    if (k >= g->n) {
      CHECK(0, "%s: %s missing", label, f[i].base);
      return;
    }
    CHECK(strcmp(g->name[k], f[i].base) == 0, "%s: name %s, want %s", label, g->name[k],
          f[i].base);
    CHECK(g->size[k] == f[i].data.len, "%s: %s size %u", label, f[i].base, g->size[k]);
    CHECK(g->data[k].len == f[i].data.len &&
              (f[i].data.len == 0 || memcmp(g->data[k].p, f[i].data.p, f[i].data.len) == 0),
          "%s: %s content differs (%zu of %zu B)", label, f[i].base, g->data[k].len,
          f[i].data.len);
    CHECK(g->ended[k] == 1, "%s: %s not ended complete (%d)", label, f[i].base, g->ended[k]);
    k++;
  }
  CHECK(k == g->n, "%s: %d files delivered, want %d", label, g->n, k);
}

/* ---- tests -------------------------------------------------------------------- */

static void test_round_trips(void) {
  struct file f[] = {
      {".", ".", MODE_DIR, {0}},
      {"./Weckton.m4a", "Weckton.m4a", MODE_FILE, random_bytes(700 * 1024 + 17)},
      {"./notes.txt", "notes.txt", MODE_FILE, text_bytes(300 * 1024 + 3)},
      {"./empty.wav", "empty.wav", MODE_FILE, {0}},
      {"./._Weckton.m4a", "._Weckton.m4a", MODE_FILE, random_bytes(82)},
      {"./x", "x", MODE_FILE, random_bytes(1)},
  };
  const int n = (int)(sizeof f / sizeof f[0]);
  const size_t pieces[] = {SIZE_MAX, 1, 3, 1000, 4096, 65537, 0, 0, 0};
  for (int newc = 0; newc <= 1; newc++) {
    struct buf a = archive_of(f, n, newc);
    for (int codec = SENDER; codec <= ALWAYS_STORE; codec++) {
      for (int end = 0; end <= 1; end++) {
        struct buf body = dvzip(&a, (enum codec)codec, end);
        for (size_t i = 0; i < sizeof pieces / sizeof pieces[0]; i++) {
          char label[96];
          snprintf(label, sizeof label, "%s codec %d end %d piece %zu", newc ? "newc" : "odc",
                   codec, end, pieces[i]);
          struct got g = {0};
          bool ok;
          AdStream *s = feed(&body, pieces[i], &g, &ok);
          CHECK(ok, "%s: %s", label, ad_stream_error_name(ad_stream_error(s)));
          check_files(&g, f, n, label);
          CHECK(ad_stream_archive_done(s), "%s: no trailer", label);
          CHECK(ad_stream_saw_end_record(s) == end, "%s: end record", label);
          CHECK(end || ad_stream_at_record_boundary(s), "%s: not at a boundary", label);
          struct AdStreamStats st;
          ad_stream_stats(s, &st);
          CHECK(st.out_bytes == a.len, "%s: %llu B out, want %zu", label,
                (unsigned long long)st.out_bytes, a.len);
          CHECK(st.records == (a.len + GRAIN - 1) / GRAIN, "%s: %u records", label, st.records);
          if (codec == SENDER) CHECK(st.stored > 0 && st.stored < st.records, "%s: mix", label);
          CHECK(st.files == 5, "%s: %u files", label, st.files);
          ad_stream_free(s);
          got_free(&g);
        }
        free(body.p);
      }
    }
    free(a.p);
  }
  for (int i = 0; i < n; i++) free(f[i].data.p);
}

/* Random letters: deflate saves about 40 %, so records stay deflated and long. */
static struct buf letter_bytes(size_t n) {
  struct buf b = {0};
  for (size_t i = 0; i < n; i++) {
    uint8_t c = (uint8_t)('a' + (rnd() >> 16) % 26);
    buf_put(&b, &c, 1);
  }
  return b;
}

static struct buf one_file_body(size_t len, struct file *out) {
  out[0] = (struct file){"./Morgen.m4a", "Morgen.m4a", MODE_FILE, letter_bytes(len)};
  struct buf a = archive_of(out, 1, false);
  struct buf body = dvzip(&a, SENDER, false);
  free(a.p);
  return body;
}

static void test_refusals(void) {
  struct file f[2] = {
      {"./a.wav", "a.wav", MODE_FILE, random_bytes(5000)},
      {"./b.wav", "b.wav", MODE_FILE, random_bytes(6000)},
  };
  struct buf a = archive_of(f, 2, false);
  struct buf body = dvzip(&a, SENDER, true);

  /* begin() refuses one file: its bytes are skipped, the other arrives. */
  struct got g = {.refuse_name = "a.wav"};
  bool ok;
  AdStream *s = feed(&body, 777, &g, &ok);
  CHECK(ok && g.n == 1 && strcmp(g.name[0], "b.wav") == 0 && g.data[0].len == 6000 &&
            g.ended[0] == 1,
        "skip: %d files", g.n);
  ad_stream_free(s);
  got_free(&g);

  /* data() refuses: the stream stops, the file ends incomplete, later input is ignored. */
  struct got h = {.refuse_after = 3000};
  s = feed(&body, 1000, &h, &ok);
  CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_SINK, "sink: %s",
        ad_stream_error_name(ad_stream_error(s)));
  CHECK(h.n == 1 && h.ended[0] == -1, "sink: %d files, ended %d", h.n, h.ended[0]);
  CHECK(!ad_stream_feed(s, body.p, 10), "sink: input after the failure");
  ad_stream_free(s);
  got_free(&h);

  free(body.p);
  free(a.p);
  free(f[0].data.p);
  free(f[1].data.p);
}

static void test_damage(void) {
  struct file f[1];
  struct buf body = one_file_body(400 * 1024, f);
  bool ok;

  /* A flipped byte inside a deflated record is always caught: by the record's adler32 or
     zlib framing, or by the archive it garbles. Where in the record it sits decides
     which; bytes before the check may already have gone out (see ad_stream.h). */
  uint32_t rec0 = (uint32_t)body.p[1] << 16 | (uint32_t)body.p[2] << 8 | body.p[3];
  CHECK(body.p[0] == 0 && rec0 > 1000 && body.len > 2 * rec0, "flip: want deflated records");
  /* Early (the zlib and block headers), inside, last bytes (the adler32), next record. */
  const size_t flips[] = {5, 200, 4 + rec0 / 2, 4 + rec0 - 1, 4 + rec0 + 4 + 300};
  for (size_t i = 0; i < sizeof flips / sizeof flips[0]; i++) {
    struct buf bad = {0};
    buf_put(&bad, body.p, body.len);
    bad.p[flips[i]] ^= 0x40;
    struct got g = {0};
    AdStream *s = feed(&bad, 512, &g, &ok);
    enum AdStreamError e = ad_stream_error(s);
    CHECK(!ok && (e == AD_STREAM_E_INFLATE || e == AD_STREAM_E_ARCHIVE), "flip at %zu: %s",
          flips[i], ad_stream_error_name(e));
    ad_stream_free(s);
    CHECK(g.n == 0 || g.ended[g.n - 1] != 0, "flip at %zu: a file left open", flips[i]);
    got_free(&g);
    free(bad.p);
  }
  AdStream *s;

  /* Cut inside a record: no error (more may come), not at a boundary; freeing ends the
     open file incomplete. */
  struct buf cut = {0};
  buf_put(&cut, body.p, body.len / 2);
  struct got c = {0};
  s = feed(&cut, 4096, &c, &ok);
  CHECK(ok && !ad_stream_at_record_boundary(s) && !ad_stream_archive_done(s), "cut: state");
  CHECK(c.n == 1 && c.ended[0] == 0, "cut: file still open");
  ad_stream_free(s);
  CHECK(c.ended[0] == -1, "cut: free did not end the file incomplete");
  got_free(&c);
  free(cut.p);

  /* Record headers no sender writes. */
  const uint8_t huge[] = {0x00, 0x20, 0x00, 0x00, 0x78, 0x9c};
  const uint8_t empty_stored[] = {0x80, 0x00, 0x00, 0x00};
  const struct {
    const uint8_t *p;
    size_t n;
  } heads[] = {{huge, sizeof huge}, {empty_stored, sizeof empty_stored}};
  for (size_t i = 0; i < 2; i++) {
    struct got h = {0};
    struct AdStreamSink sink = {sink_begin, sink_data, sink_end, &h};
    s = ad_stream_new(&sink);
    CHECK(!ad_stream_feed(s, heads[i].p, heads[i].n) &&
              ad_stream_error(s) == AD_STREAM_E_RECORD,
          "header %zu: %s", i, ad_stream_error_name(ad_stream_error(s)));
    ad_stream_free(s);
  }

  /* Not a cpio archive. */
  struct buf junk = text_bytes(1000);
  struct buf jb = dvzip(&junk, ALWAYS_STORE, true);
  struct got j = {0};
  s = feed(&jb, 100, &j, &ok);
  CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_ARCHIVE && j.n == 0, "junk: %s",
        ad_stream_error_name(ad_stream_error(s)));
  ad_stream_free(s);
  free(junk.p);
  free(jb.p);

  /* A name longer than any sender writes. */
  char longname[320];
  memset(longname, 'n', sizeof longname - 1);
  longname[sizeof longname - 1] = '\0';
  struct buf la = {0};
  odc_add(&la, longname, MODE_FILE, (const uint8_t *)"x", 1);
  struct buf lb = dvzip(&la, ALWAYS_STORE, true);
  struct got l = {0};
  s = feed(&lb, 64, &l, &ok);
  CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_ARCHIVE, "long name: %s",
        ad_stream_error_name(ad_stream_error(s)));
  ad_stream_free(s);
  free(la.p);
  free(lb.p);

  /* An odc file size above 32 bits (11 octal digits hold 33). */
  struct buf oa = {0};
  buf_printf(&oa, "070707%06o%06o%06o%06o%06o%06o%06o%011o%06o%011llo", 0, 1, MODE_FILE, 501, 20,
             1, 0, 0, 2u, 077777777777ull);
  buf_put(&oa, "x", 2);
  struct buf ob = dvzip(&oa, ALWAYS_STORE, true);
  struct got o = {0};
  s = feed(&ob, 64, &o, &ok);
  CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_ARCHIVE && o.n == 0, "33-bit size: %s",
        ad_stream_error_name(ad_stream_error(s)));
  ad_stream_free(s);
  free(oa.p);
  free(ob.p);

  free(body.p);
  free(f[0].data.p);
}

/* A record whose bytes end before its zlib stream does fails at the record's end, with
   no output beyond what its bytes hold. */
static void test_truncated_record(void) {
  struct file f[1];
  struct buf body = one_file_body(400 * 1024, f);
  uint32_t rec0 = (uint32_t)body.p[1] << 16 | (uint32_t)body.p[2] << 8 | body.p[3];
  CHECK(body.p[0] == 0 && rec0 > 1000, "short record: want a deflated record");
  const uint32_t keep[] = {2, 100, rec0 / 2, rec0 - 4, rec0 - 1};
  const size_t pieces[] = {SIZE_MAX, 1, 512};
  for (size_t i = 0; i < sizeof keep / sizeof keep[0]; i++) {
    struct buf cut = {0};
    put_be32(&cut, keep[i]);
    buf_put(&cut, body.p + 4, keep[i]);
    for (size_t j = 0; j < sizeof pieces / sizeof pieces[0]; j++) {
      struct got g = {0};
      bool ok;
      AdStream *s = feed(&cut, pieces[j], &g, &ok);
      CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_INFLATE, "short record %u/%zu: %s", keep[i],
            pieces[j], ad_stream_error_name(ad_stream_error(s)));
      struct AdStreamStats st;
      ad_stream_stats(s, &st);
      CHECK(st.out_bytes <= GRAIN && st.records == 0, "short record %u/%zu: %llu B out", keep[i],
            pieces[j], (unsigned long long)st.out_bytes);
      CHECK(g.n == 0 || g.ended[g.n - 1] == -1, "short record %u/%zu: a file left open", keep[i],
            pieces[j]);
      ad_stream_free(s);
      got_free(&g);
    }
    free(cut.p);
  }
  free(body.p);
  free(f[0].data.p);
}

/* ---- the size limit ---------------------------------------------------------------- */

/* A sink that only counts: these transfers are too large to keep. */
struct counted {
  int begun, complete, incomplete;
  uint64_t bytes;
};

static bool count_begin(void *ctx, const char *name, uint32_t size) {
  (void)name, (void)size;
  ((struct counted *)ctx)->begun++;
  return true;
}

static bool count_data(void *ctx, const uint8_t *data, size_t len) {
  (void)data;
  ((struct counted *)ctx)->bytes += len;
  return true;
}

static void count_end(void *ctx, bool complete) {
  struct counted *c = ctx;
  if (complete) c->complete++;
  else c->incomplete++;
}

/* An odc header for a regular file of `size` bytes, as a stored record. */
static void put_entry(struct buf *body, const char *name, uint32_t size) {
  struct buf h = {0};
  size_t ns = strlen(name) + 1;
  buf_printf(&h, "070707%06o%06o%06o%06o%06o%06o%06o%011o%06o%011o", 0, 1, MODE_FILE, 501, 20, 1,
             0, 0, (unsigned)ns, (unsigned)size);
  buf_put(&h, name, ns);
  put_be32(body, 0x80000000u | (uint32_t)h.len);
  buf_put(body, h.p, h.len);
  free(h.p);
}

static void test_size_limit(void) {
  struct counted c = {0};
  struct AdStreamSink sink = {count_begin, count_data, count_end, &c};

  /* An entry announcing more than the limit is refused before the sink hears of it;
     one of exactly the limit is announced. */
  for (uint32_t over = 0; over <= 1; over++) {
    struct buf body = {0};
    put_entry(&body, "./big.wav", AD_STREAM_MAX_BYTES + over);
    c = (struct counted){0};
    AdStream *s = ad_stream_new(&sink);
    bool ok = ad_stream_feed(s, body.p, body.len);
    CHECK(ok == !over && c.begun == (over ? 0 : 1), "entry +%u: ok %d, begun %d", over, ok,
          c.begun);
    CHECK(ad_stream_error(s) == (over ? AD_STREAM_E_SIZE : AD_STREAM_OK), "entry +%u: %s", over,
          ad_stream_error_name(ad_stream_error(s)));
    ad_stream_free(s);
    free(body.p);
  }

  /* Two files of 130 MiB, each within the limit, together over it: the stream stops
     inside the second. The file bytes are zeros, one deflated grain sent again and again
     (about 150 bytes per 128 KiB). */
  const uint32_t grains = 130u * 1024 * 1024 / GRAIN;
  struct buf zeros = {0};
  uint8_t *z = calloc(1, GRAIN);
  buf_put(&zeros, z, GRAIN);
  free(z);
  struct buf one = dvzip(&zeros, ALWAYS_DEFLATE, false);
  CHECK(one.len < 1000, "total: a grain of zeros deflates to %zu B", one.len);
  struct buf body = {0};
  for (int i = 0; i < 2; i++) {
    put_entry(&body, i ? "./b.wav" : "./a.wav", grains * GRAIN);
    for (uint32_t k = 0; k < grains; k++) buf_put(&body, one.p, one.len);
  }
  c = (struct counted){0};
  AdStream *s = ad_stream_new(&sink);
  bool ok = true;
  for (size_t off = 0; ok && off < body.len; off += 4096)
    ok = ad_stream_feed(s, body.p + off, body.len - off < 4096 ? body.len - off : 4096);
  struct AdStreamStats st;
  ad_stream_stats(s, &st);
  CHECK(!ok && ad_stream_error(s) == AD_STREAM_E_SIZE, "total: %s",
        ad_stream_error_name(ad_stream_error(s)));
  CHECK(c.begun == 2 && c.complete == 1 && c.incomplete == 1, "total: %d begun, %d complete",
        c.begun, c.complete);
  CHECK(c.bytes <= AD_STREAM_MAX_BYTES && st.out_bytes <= AD_STREAM_MAX_BYTES + GRAIN,
        "total: %llu B delivered, %llu B out", (unsigned long long)c.bytes,
        (unsigned long long)st.out_bytes);
  CHECK(st.in_bytes < body.len, "total: the whole body was read");
  ad_stream_free(s);
  free(body.p);
  free(one.p);
  free(zeros.p);
}

/* A song-sized transfer: memory stays bounded however large the file is. */
static void test_large(void) {
  struct file f[1] = {{"./Lied.flac", "Lied.flac", MODE_FILE, random_bytes(12u << 20)}};
  struct buf a = archive_of(f, 1, false);
  struct buf body = dvzip(&a, SENDER, false);
  struct got g = {0};
  bool ok;
  AdStream *s = feed(&body, 0, &g, &ok);
  CHECK(ok, "large: %s", ad_stream_error_name(ad_stream_error(s)));
  check_files(&g, f, 1, "large");
  ad_stream_free(s);
  got_free(&g);
  free(body.p);
  free(a.p);
  free(f[0].data.p);
}

int main(void) {
  test_round_trips();
  test_refusals();
  test_damage();
  test_truncated_record();
  test_size_limit();
  test_large();
  if (g_failed) {
    fprintf(stderr, "stream_test: %d checks FAILED\n", g_failed);
    return 1;
  }
  printf("stream_test: all checks passed\n");
  return 0;
}
