#!/bin/sh
# Host test of the streaming decoder (components/esp32drop/stream), with
# sanitizers. Needs ./fetch.sh first (vendor/miniz).
set -eu
cd "$(dirname "$0")"
out="${TMPDIR:-/tmp}/wakey_stream_test"
cc -std=c11 -O1 -g -Wall -Wextra -Werror -fsanitize=address,undefined \
	-I. -I../components/esp32drop/stream \
	stream_test.c ../components/esp32drop/stream/ad_stream.c miniz_impl.c \
	-o "$out"
exec "$out"
