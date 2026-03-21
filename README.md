# scrolltune

A lightweight evdev filter that tames high-resolution scroll wheels on Linux.

## Problem

Mice with high-resolution scroll wheels (Logitech MX Master, MX Anywhere, etc.) feel jittery on Linux. Every hi-res scroll tick is passed straight through to applications, causing overly sensitive, twitchy scrolling. On Windows and macOS, scrolling typically feels closer to the wheel's physical detents because small ticks are accumulated before a full line-scroll step is emitted.

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

`scrolltune` only supports mice that expose `REL_WHEEL_HI_RES`. If the selected device does not provide hi-res wheel events, the program exits with a clear error instead of grabbing it.

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
sudo scrolltune
```

This is the quickest way to test the filter without setting it up as a persistent system service.

## System installation

If you want `scrolltune` to run automatically in the background, use the install script to build it, install the binary to `/usr/local/bin`, install the systemd service, seed `/etc/scrolltune.toml` if it does not already exist, and start it:

```
./install.sh
```

To intentionally replace the existing config with the repo default:

```
./install.sh --overwrite-config
```

This installs the persistent systemd-based setup. The service runs as root (required for evdev grab and uinput access) and starts automatically on boot.

The install script also installs a udev rule that restarts the service when a mouse-like USB input device is plugged in or removed, so the filter recovers automatically when the device topology changes. The service also remembers the last device it successfully grabbed and uses that to disambiguate future restarts when possible.

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
