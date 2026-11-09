# Tab5 alarm clock

The [development concept](architecture/developtment%20concept.md) describes the planned M5Stack Tab5 alarm clock. Rust-only is a project constraint: do not add project-authored C/C++ or C/C++ board components/drivers.

The firmware lives in [`firmware/`](firmware/README.md), its own Cargo workspace: Rust (std) on ESP-IDF 5.5.5 with the Embassy executor. It boots on the Tab5 and drives the MIPI-DSI display. It includes Slint and Buoyant GUI tests with measured frame rates. The Tab5 here has an ESP32-P4 **rev v1.3**, and the no_std `esp-hal` supports only rev ≥ v3.0. That is why the firmware uses ESP-IDF rather than esp-hal; see the firmware README.

The isolated [Tab5 P4 Rust smoke test](spikes/tab5-p4-rust-smoke/README.md) is a build-only `no_std` `esp-hal` image. It targets P4 rev ≥ v3 and will not run on the rev v1.3 board.

The user reports successful Rust + `espup` display/touch bring-up on an M5Stack CoreS3 Lite in a separate project. This validates that board/toolchain workflow, not the Tab5's different ESP32-P4, MIPI-DSI display, or touch wiring. The Tab5 uses RISC-V P4 and C6 targets; the CoreS3 uses Xtensa S3 and needs the ESP-RS toolchain installed with `espup`.

The working TIDAL audio downloader lives in [`spikes/tidal-download`](spikes/tidal-download). It is an independent Cargo workspace with its own lockfile and pinned host Rust toolchain.

Run it in the dev container:

```sh
cd spikes/tidal-download
cargo run
cargo run -- doctor
```

See the [downloader README](spikes/tidal-download/README.md) for login, download, quality selection, WAV conversion, and offline verification instructions.

The saved lossless FLAC has passed both automated offline verification and the user's manual offline listening check. It is 44.1 kHz; a 48 kHz WAV was not generated, and no audio has yet been played by Tab5 firmware.

The existing saved login and downloaded audio moved to `spikes/tidal-download/.local/`. Build artifacts still use `/tmp/cargo-target` as configured by the dev container. Run Cargo inside the spike so its host toolchain is selected independently of the future firmware toolchain.
