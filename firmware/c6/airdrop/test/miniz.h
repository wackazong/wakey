/* SPDX-FileCopyrightText: 2026 Alexander Walter
 * SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * The host test's tinfl/tdefl: miniz 1.14 (vendor/miniz), the same API and code
 * generation as the C6's ROM copy (esp_rom/include/miniz.h).
 */
#pragma once
#define MINIZ_NO_STDIO
#define MINIZ_NO_TIME
#define MINIZ_NO_ARCHIVE_APIS
#define MINIZ_NO_ZLIB_COMPATIBLE_NAMES
#define MINIZ_HEADER_FILE_ONLY
#include "../vendor/miniz/miniz.c"
#undef MINIZ_HEADER_FILE_ONLY
