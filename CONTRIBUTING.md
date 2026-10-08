# Contributing to Nivel

Thanks for helping people sound better. Bug reports from real setups are as valuable as code: every mic, headset and audio interface behaves a little differently.

## Reporting a problem

Open an [issue](https://github.com/machina-sports/nivel/issues/new/choose) and include the output of:

```bash
nivel --version
nivel devices
nivel doctor
```

`nivel doctor` prints levels only; it doesn't record or upload any audio.

## Development

You need [Rust](https://rustup.rs) 1.88 or newer. On Debian and Ubuntu, also `sudo apt install libasound2-dev pkg-config`.

```bash
cargo test --all                         # unit tests, no audio hardware needed
cargo clippy --all-targets -- -D warnings
cargo fmt --all
cargo run -p nivel-cli -- doctor         # try it on your own mic
cargo run -p nivel-cli -- process in.wav out.wav
```

`nivel process` is the quickest way to hear a DSP change: record a WAV, process it, compare.

## Where things live

- `crates/nivel-dsp`: pure processing. Every processor works on 480-sample frames at 48 kHz, allocates nothing after construction and has unit tests with synthetic signals. DSP changes should come with a test that shows the behavior.
- `crates/nivel-core`: devices, the real-time engine, the watchdog and supervisor, OS volume backends (`os_volume/{macos,windows,linux}.rs`) and the virtual mic.
- `crates/nivel-cli`: the `nivel` command and its terminal output.

[docs/how-it-works.md](docs/how-it-works.md) explains the design.

## Real-time rules

Code that runs inside an audio callback (`engine.rs`: `open_input`, `open_output`, `Downmix`) must not allocate, lock, log, or call into the OS. Push the work to the DSP thread instead.

## Pull requests

- Keep each PR focused on one change, and describe how you tested it (which OS, which mic).
- CI runs formatting, clippy and tests on Linux, macOS and Windows; please make sure they pass.
- By contributing, you agree that your work is released under the [MIT License](LICENSE), like the rest of the project.

## Questions

Ask in an [issue](https://github.com/machina-sports/nivel/issues), or write to Mateus Pinheiro at [mateus.pinheiro@machina.gg](mailto:mateus.pinheiro@machina.gg).

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
