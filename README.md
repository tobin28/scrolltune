# scrolltune

A lightweight filter that tames high-resolution scroll wheels on Linux.

## Problem

Mice with high-resolution scroll wheels (Logitech MX Master, MX Anywhere, etc.) feel jittery on Linux. Every hi-res tick is forwarded directly to applications — unlike Windows and macOS, Linux has no built-in accumulation, so scrolling feels overly sensitive.

## How it works

The filter grabs the mouse's evdev device, intercepts hi-res scroll events (`REL_WHEEL_HI_RES`), and accumulates them. A scroll event is only emitted when the accumulated value reaches a configurable threshold (default: 120 hi-res ticks = one physical notch/detent). On direction change, the accumulator resets to zero for immediate responsiveness. All other events (mouse movement, clicks, horizontal scroll) pass through instantly — there is no buffering or added latency.

The filter creates a virtual input device via uinput that applications see instead of the original. It uses standard kernel interfaces (evdev and uinput) and works on any modern Linux distribution.

`scrolltune` only supports mice that expose `REL_WHEEL_HI_RES`. If the selected device does not provide hi-res wheel events, the program exits with a clear error.

## Device detection

The filter uses a priority chain to find the mouse:

1. **Explicit name** — if `device` is set in the config, it matches by name (case-insensitive substring)
2. **Single supported mouse** — if only one mouse with `REL_WHEEL_HI_RES` is found, it is used automatically
3. **Last known device** — if multiple supported mice are found, it prefers the last device that was successfully attached and grabbed
4. **Known vendor** — if that still does not disambiguate, it filters by known mouse vendor IDs (currently Logitech). If exactly one matches, it is used
5. **Manual fallback** — if none of the above work, all supported candidates are printed so you can set `device` in the config

Virtual input devices (uinput, ydotool, etc.) are excluded automatically.

## Requirements

- Rust toolchain (`cargo`)
- Linux kernel with evdev and uinput support (standard on all modern distros)

## Building

```
cargo build --release
```

## Configuration

Settings are read from `/etc/scrolltune.toml` (or `scrolltune.toml` in the working directory as fallback).

```toml
# Device name to match (case-insensitive substring)
# Must refer to a mouse that exposes REL_WHEEL_HI_RES
# Leave empty for auto-detection
device = ""

# Hi-res tick threshold per scroll notch (120 = one physical detent)
# Higher values = less sensitive, lower = more sensitive
# Try 60 for smoother, slightly more responsive scrolling
# Must be a positive integer
threshold = 120
```

In most cases you can leave `device` empty — auto-detection will find the right supported mouse. Set it only if auto-detection picks the wrong device or can't decide.

## Manual usage

```
# Uses /etc/scrolltune.toml or defaults
cargo run --release -- --help

# Start the filter directly from the build tree
sudo cargo run --release --
```

This is the quickest way to test the filter without setting it up as a persistent system service.

For quick inspection and configuration, `scrolltune` also provides a small CLI:

```
# Show available commands
cargo run --release -- --help

# Show service/config status
cargo run --release -- --status

# List mouse-like input devices
cargo run --release -- --list-devices

# Pick a device interactively
sudo cargo run --release -- --set-device

# Change threshold interactively or directly
sudo cargo run --release -- --set-threshold
sudo cargo run --release -- --set-threshold 60
```

Short aliases are available as well: `-h`, `-s`, `-l`, `-d`, and `-t`. Use `sudo` for commands that update `/etc/scrolltune.toml` or restart the systemd service. After installation, the same commands are available through the installed `scrolltune` binary in `/usr/local/bin`.

When these commands update `/etc/scrolltune.toml`, `scrolltune.service` is restarted automatically so the change takes effect immediately.

## System installation

To run `scrolltune` as a persistent background service, use the install script. It builds the binary, installs it to `/usr/local/bin`, sets up the systemd service and udev rule, seeds `/etc/scrolltune.toml` if missing, and starts the service:

```
./install.sh
./install.sh --overwrite-config  # replace existing config with repo default
```

The service runs as root (required for evdev grab and uinput) and starts on boot. A udev rule restarts it when mouse-like input devices are added or removed (USB and Bluetooth), and it remembers the last device it grabbed to disambiguate future restarts.
