# Source this (`. ./env.sh`) to build with the firmware's own ESP-IDF
# (firmware/.embuild/espressif, installed by the firmware's first build).
# ESP-IDF's export.sh fails there (embuild installs only the tools the
# firmware needs), so the environment is set by hand, as in
# firmware/c6/README.md.

E="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/../../firmware/.embuild/espressif" && pwd)"
export IDF_PATH="$E/esp-idf/v5.5.5"
export IDF_TOOLS_PATH="$E"
export ESP_IDF_VERSION=5.5
export IDF_PYTHON_ENV_PATH="$E/python_env/idf5.5_py3.14_env"
export PATH="$IDF_PYTHON_ENV_PATH/bin:$E/tools/riscv32-esp-elf/esp-14.2.0_20260121/riscv32-esp-elf/bin:$E/tools/cmake/3.30.2/CMake.app/Contents/bin:$E/tools/ninja/1.12.1:$PATH"
alias idf.py='python "$IDF_PATH/tools/idf.py"'
