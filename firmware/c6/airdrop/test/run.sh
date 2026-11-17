#!/bin/sh
# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Host test of the streaming decoder (components/esp32drop/stream), with
# sanitizers. Needs ./fetch.sh first (vendor/miniz).
set -eu
cd "$(dirname "$0")"
out="${TMPDIR:-/tmp}/wakey_stream_test"
set -- -std=c11 -O1 -g -fsanitize=address,undefined -I. -I../components/esp32drop/stream
# miniz is third-party and built without warnings (-w: GCC does not take
# miniz_impl.c's pragma for -Wall); our own files with -Werror.
cc "$@" -w -c miniz_impl.c -o "$out.miniz.o"
cc "$@" -Wall -Wextra -Werror \
	stream_test.c ../components/esp32drop/stream/ad_stream.c "$out.miniz.o" \
	-o "$out"
exec "$out"
