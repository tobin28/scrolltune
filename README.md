# scrolltune

A lightweight evdev filter that tames high-resolution scroll wheels on Linux.

## Problem

Mice with high-resolution scroll wheels (Logitech MX Master, MX Anywhere, etc.) feel jittery on Linux. Every tiny hi-res scroll tick is passed straight through to applications, causing overly sensitive, twitchy scrolling. On Windows, applications accumulate these small ticks and only scroll once a full notch threshold is reached, matching the tactile feedback of the wheel.

Linux has no built-in mechanism for this — libinput forwards all hi-res events directly, leaving applications to deal with them individually. Most Linux applications don't implement accumulation, resulting in noticeably worse scroll behavior compared to the same mouse on Windows or macOS.

## How it works

The filter grabs the mouse's evdev device, intercepts hi-res scroll events (`REL_WHEEL_HI_RES`), and accumulates them. A scroll event is only emitted when the accumulated value reaches a configurable threshold (default: 120 hi-res ticks = one physical notch/detent).

- Hi-res scroll ticks are accumulated until the threshold is reached
- When the threshold is crossed, quantized `REL_WHEEL` and `REL_WHEEL_HI_RES` events are emitted
- On direction change, the accumulator resets to zero for immediate responsiveness
- The original paired `REL_WHEEL` events from the kernel are suppressed

All other events (mouse movement, clicks, horizontal scroll) pass through with zero delay. There is no buffering or timer — each event is processed instantly, so there is no added input latency.

The filter creates a virtual input device via uinput that applications see instead of the original.

It uses standard Linux kernel interfaces (evdev and uinput) and works on any distribution — there are no distro-specific or desktop-specific dependencies.

## Device detection

The filter uses a priority chain to find the mouse:

1. **Explicit name** — if `device` is set in the config, it matches by name (case-insensitive substring)
2. **Single mouse** — if only one mouse-like device is found, it is used automatically
3. **Known vendor** — if multiple mice are found, it filters by known mouse vendor IDs (currently Logitech). If exactly one matches, it is used
4. **Manual fallback** — if none of the above work, all candidates are printed so you can set `device` in the config

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
# Leave empty for auto-detection
device = ""

# Hi-res tick threshold per scroll notch (120 = one physical detent)
# Higher values = less sensitive, lower = more sensitive
threshold = 120
```

In most cases you can leave `device` empty — auto-detection will find the right mouse. Set it only if auto-detection picks the wrong device or can't decide.

## Installation

Run the deploy script to build, install the binary to `/usr/local/bin`, install the config and systemd service, and start it:

```
./deploy.sh
```

The service runs as root (required for evdev grab and uinput access) and starts automatically on boot.

The deploy script also installs a udev rule that restarts the service when a USB HID device is plugged in, so the filter recovers automatically if the mouse is unplugged and reconnected.

## Manual usage

```
# Uses /etc/scrolltune.toml or defaults
sudo scrolltune
```

## Managing the service

```
# Check status
sudo systemctl status scrolltune

# Restart after config change
sudo systemctl restart scrolltune

# Stop
sudo systemctl stop scrolltune

# Disable autostart
sudo systemctl disable scrolltune
```
