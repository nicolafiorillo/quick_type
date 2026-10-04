# Quick Type

A MacOS menu bar utility: hold a vowel (a/e/i/o/u/A/E/I/O/U) and press Space to replace it with its accented counterpart (à/è/ì/ò/ù/À/È/Ì/Ò/Ù).

Inspired by PowerToys Quick Accent.

## Requirements

- MacOS with **Accessibility** permission (System Settings → Privacy & Security → Accessibility). When launched from a terminal it is the terminal's permission; with **Launch at login** it must be granted to `~/.cargo/bin/quick_type` itself (the app asks on first launch)
- Rust (2024 edition)

## Build & install

```sh
make build    # optimised release build
make test     # run tests
make install  # install to ~/.cargo/bin
```

## Usage

Run `quick_type`: an "è" icon appears in the menu bar. Open **Settings...** to configure.

Architecture and conventions: [AGENTS.md](AGENTS.md).
