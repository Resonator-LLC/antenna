# Contributing to Antenna

## Prerequisites

- Rust stable (edition 2021)
- C11 compiler (gcc or clang)
- pkg-config
- System libraries: serd, toxcore, quickjs, libsodium, opus, libvpx

### macOS

```bash
brew install serd toxcore quickjs libsodium opus libvpx pkg-config
```

### Debian/Ubuntu

```bash
sudo apt install build-essential pkg-config libserd-dev libtoxcore-dev \
  quickjs libsodium-dev libopus-dev libvpx-dev
```

## Getting Started

```bash
git clone --recursive https://source.resonator.network/resonator/antenna.git
cd antenna
```

The `--recursive` flag fetches the carrier submodule (in `third_party/carrier`).
Serd, toxcore, and QuickJS are linked as system libraries via pkg-config.

## Build

```bash
make build
# or
cargo build --release
```

## Test

```bash
make test
# or
cargo test
```

## Code Style

- Run `cargo fmt` before submitting
- Run `cargo clippy` and fix warnings
- Add `// SAFETY:` comments to all `unsafe` blocks
- Keep functions short and focused

## Pull Requests

1. Fork the repository
2. Create a feature branch from `main`
3. Make your changes
4. Run `make lint` to check formatting and clippy
5. Run `make test` to verify tests pass
6. Submit a pull request with a clear description

## License

By contributing, you agree that your contributions will be licensed under
the MIT license (see LICENSE).
