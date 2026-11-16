# SPDX-FileCopyrightText: 2026 Alexander Walter
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Source this (`. firmware/c6/env.sh`, bash or zsh) to build the C6 images and
# rescue/ with the firmware's own ESP-IDF 5.5.5 (firmware/.embuild/espressif,
# installed by the firmware's first build). ESP-IDF's export.sh fails there:
# embuild installs only the tools the firmware needs, and export.sh wants all
# of them. So the environment is set from what is installed.

_wk_env() {
	_wk_self=
	if [ -n "${BASH_VERSION:-}" ]; then
		eval '_wk_self=${BASH_SOURCE[0]}'
	elif [ -n "${ZSH_VERSION:-}" ]; then
		eval '_wk_self=${(%):-%x}'
	else
		echo "env.sh: source it from bash or zsh" >&2
		return 1
	fi
	_wk_e=$(cd "$(dirname "$_wk_self")/../.embuild/espressif" 2>/dev/null && pwd) || {
		echo "env.sh: firmware/.embuild/espressif not found: build the firmware once first" >&2
		return 1
	}
	# Tool versions differ between hosts and embuild releases: take what is
	# there (the last in sorted order if there are several), found with find:
	# zsh fails on a glob without a match.
	_wk_find() { find "$_wk_e/$1" -maxdepth "$2" "$3" "$4" -name "$5" 2>/dev/null | sort | tail -n 1; }
	_wk_py=$(_wk_find python_env 1 -type d 'idf5.5_py*_env')
	_wk_gcc=$(_wk_find tools/riscv32-esp-elf 4 -type f riscv32-esp-elf-gcc)
	_wk_cmake=$(_wk_find tools/cmake 5 -type f cmake)
	_wk_ninja=$(_wk_find tools/ninja 2 -type f ninja)
	unset -f _wk_find
	if [ ! -d "$_wk_e/esp-idf/v5.5.5" ] || [ -z "$_wk_py" ] || [ -z "$_wk_gcc" ] ||
		[ -z "$_wk_cmake" ] || [ -z "$_wk_ninja" ]; then
		echo "env.sh: incomplete ESP-IDF in $_wk_e (esp-idf/v5.5.5, python_env, riscv32-esp-elf, cmake, ninja)" >&2
		return 1
	fi
	export IDF_PATH="$_wk_e/esp-idf/v5.5.5"
	export IDF_TOOLS_PATH="$_wk_e"
	export ESP_IDF_VERSION=5.5
	export IDF_PYTHON_ENV_PATH="$_wk_py"
	PATH="$_wk_py/bin:$(dirname "$_wk_gcc"):$(dirname "$_wk_cmake"):$(dirname "$_wk_ninja"):$PATH"
	export PATH
}
_wk_env
_wk_rc=$?
unset -f _wk_env
unset _wk_self _wk_e _wk_py _wk_gcc _wk_cmake _wk_ninja
[ "$_wk_rc" -eq 0 ] && unset _wk_rc
