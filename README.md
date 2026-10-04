# Quick Accent (`quick_type`)

A macOS menu bar utility: hold a vowel (A/E/I/O/U) and press Space to replace it with its accented counterpart (à/è/ì/ò/ù). Inspired by PowerToys Quick Accent.

## Requirements

- macOS with **Accessibility** permission (System Settings → Privacy & Security → Accessibility). When launched from a terminal it is the terminal's permission; with **Launch at login** it must be granted to `~/.cargo/bin/quick_type` itself (the app asks on first launch)
- Rust (2024 edition)

## Build & install

```sh
make build    # optimised release build
make install  # install to ~/.cargo/bin
make test     # run tests
```

## Usage

Run `quick_type`: an "è" icon appears in the menu bar. From there you can check the version, toggle **Launch at login**, and quit with **Quit**.

Architecture and conventions: [AGENTS.md](AGENTS.md).
