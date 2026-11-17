# Contributing

Thanks for your interest in Wakey. Issues and pull requests are welcome: bug reports with a log from the device, support for more hardware variants, and improvements to the code or the documentation.

## Before you start

- Read [docs/architecture.md](docs/architecture.md) for the design and [docs/pitfalls.md](docs/pitfalls.md) before changing an area: many choices that look arbitrary are workarounds for the hardware.
- For larger changes, open an issue first to agree on the approach.

## Workspaces

Each directory with a `Cargo.toml` is its own workspace with its own toolchain; run Cargo from inside it.

| Directory | Toolchain | Check |
| --- | --- | --- |
| `firmware/` | pinned nightly, ESP32-P4 target (`rust-toolchain.toml`) | `cargo fmt --check`, `cargo clippy --release --bins` (with all features too when feature-gated code changed; see [firmware/README.md](firmware/README.md#build-and-flash)) |
| `wakey-core/` | stable | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |
| `tools/ui-preview/` | stable | `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo run --release -- <out dir>` renders every page and checks the list rows; on Linux it needs fontconfig (`libfontconfig1-dev`) |

The C6 images (`firmware/c6/`) are C on ESP-IDF; see [firmware/c6/README.md](firmware/c6/README.md). Their stream decoder has a host test: `firmware/c6/airdrop/test/run.sh` after `./fetch.sh`.

CI runs all of these except the firmware's Clippy, which needs the ESP-IDF build: run that one locally.

## Guidelines

- Keep hardware-independent logic in `wakey-core` with tests; the firmware wires it to the board.
- No text for the screen in the code: every text is a key in each language file in `wakey-core/lang/` ([how](firmware/README.md#how-to-)).
- Comments explain why, not what; measured numbers that justify a choice belong next to it.
- Run the checks above, and run what you changed on a Tab5 when it touches the firmware. Say in the pull request what you tested on the device.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org): `type(scope): description`, e.g. `fix(audio): …`.
- In Markdown, write one line per paragraph.
- A trap you ran into on the hardware goes into [docs/pitfalls.md](docs/pitfalls.md).

## License

Unless you state otherwise, any contribution you submit for inclusion is licensed as MIT OR Apache-2.0, like the rest of the project, without additional terms or conditions.
