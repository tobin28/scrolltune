use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AbsInfo, AbsoluteAxisType, Device, EventType, InputEvent, RelativeAxisType};
use serde::Deserialize;
use std::path::Path;
use std::{fs, process};

const CONFIG_PATHS: &[&str] = &[
    "/etc/scrolltune.toml",
    "scrolltune.toml",
];

/// scrolltune — scroll wheel filter for mice on Linux.
///
/// Grabs the mouse's evdev device, accumulates hi-res scroll ticks,
/// and only emits scroll events when a full notch threshold is reached,
/// preventing jittery scrolling from high-resolution scroll wheels.

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_device")]
    device: String,
    #[serde(default = "default_threshold")]
    threshold: i32,
}

fn default_device() -> String { String::new() }
fn default_threshold() -> i32 { 120 }

impl Default for Config {
    fn default() -> Self {
        Self {
            device: default_device(),
            threshold: default_threshold(),
        }
    }
}

fn load_config() -> Config {
    for path in CONFIG_PATHS {
        if Path::new(path).exists() {
            match fs::read_to_string(path) {
                Ok(contents) => match toml::from_str(&contents) {
                    Ok(config) => {
                        eprintln!("Config loaded from: {path}");
                        return config;
                    }
                    Err(e) => {
                        eprintln!("Failed to parse {path}: {e}");
                        process::exit(1);
                    }
                },
                Err(e) => eprintln!("Failed to read {path}: {e}"),
            }
        }
    }
    eprintln!("No config file found, using defaults");
    Config::default()
}

fn is_mouse(dev: &Device) -> bool {
    // Exclude virtual devices (uinput, ydotool, etc.)
    if dev.input_id().bus_type() == evdev::BusType::BUS_VIRTUAL {
        return false;
    }
    dev.supported_events().contains(EventType::RELATIVE)
        && dev
            .supported_relative_axes()
            .map(|axes| {
                axes.contains(RelativeAxisType::REL_X)
                    && axes.contains(RelativeAxisType::REL_Y)
                    && axes.contains(RelativeAxisType::REL_WHEEL)
            })
            .unwrap_or(false)
}

/// Known mouse vendor IDs for auto-detection when multiple mice are found.
const KNOWN_MOUSE_VENDORS: &[(u16, &str)] = &[
    (0x046d, "Logitech"),
];

fn is_known_mouse_vendor(dev: &Device) -> bool {
    let vendor = dev.input_id().vendor();
    KNOWN_MOUSE_VENDORS.iter().any(|(id, _)| *id == vendor)
}

fn find_device(name_filter: &str) -> Option<Device> {
    // Priority 1: explicit name filter from config
    if !name_filter.is_empty() {
        return evdev::enumerate()
            .find(|(_, dev)| {
                dev.name()
                    .map(|n| n.to_lowercase().contains(&name_filter.to_lowercase()))
                    .unwrap_or(false)
                    && is_mouse(dev)
            })
            .map(|(_, dev)| dev);
    }

    let mice: Vec<_> = evdev::enumerate()
        .filter(|(_, dev)| is_mouse(dev))
        .collect();

    if mice.is_empty() {
        eprintln!("No mouse devices found");
        return None;
    }

    // Priority 2: only one mouse detected
    if mice.len() == 1 {
        let (_, dev) = mice.into_iter().next().unwrap();
        eprintln!("Auto-detected: {}", dev.name().unwrap_or("unknown"));
        return Some(dev);
    }

    // Priority 3: filter by known mouse vendors
    let known: Vec<_> = mice.iter()
        .filter(|(_, dev)| is_known_mouse_vendor(dev))
        .collect();

    if known.len() == 1 {
        let (path, _) = known[0];
        let path = path.clone();
        // Find and return the owned Device from mice
        let (_, dev) = mice.into_iter().find(|(p, _)| *p == path).unwrap();
        let vendor = dev.input_id().vendor();
        let vendor_name = KNOWN_MOUSE_VENDORS.iter()
            .find(|(id, _)| *id == vendor)
            .map(|(_, name)| *name)
            .unwrap_or("unknown");
        eprintln!(
            "Auto-detected by vendor ({}): {}",
            vendor_name,
            dev.name().unwrap_or("unknown")
        );
        return Some(dev);
    }

    // Give up — print candidates to help the user configure
    eprintln!("Multiple mice found — set 'device' in config:");
    for (_, dev) in &mice {
        let vendor = dev.input_id().vendor();
        eprintln!(
            "  - {} (vendor: {:04x})",
            dev.name().unwrap_or("unknown"),
            vendor
        );
    }
    None
}

fn build_virtual_device(source: &Device) -> VirtualDevice {
    let name = format!("{} (scrolltune)", source.name().unwrap_or("mouse"));
    let mut builder = VirtualDeviceBuilder::new()
        .expect("Failed to create virtual device builder")
        .name(&name);

    if let Some(keys) = source.supported_keys() {
        builder = builder.with_keys(&keys).expect("Failed to set keys");
    }

    if let Some(rel_axes) = source.supported_relative_axes() {
        builder = builder
            .with_relative_axes(&rel_axes)
            .expect("Failed to set relative axes");
    }

    if let Some(abs_axes) = source.supported_absolute_axes() {
        let mut abs_setup = Vec::new();
        for axis in abs_axes.iter() {
            if let Ok(info) = source.get_abs_state() {
                if let Some(raw) = info.get(axis.0 as usize) {
                    let abs_info = AbsInfo::new(
                        raw.value,
                        raw.minimum,
                        raw.maximum,
                        raw.fuzz,
                        raw.flat,
                        raw.resolution,
                    );
                    abs_setup.push(evdev::UinputAbsSetup::new(
                        AbsoluteAxisType(axis.0),
                        abs_info,
                    ));
                }
            }
        }
        for setup in abs_setup {
            builder = builder
                .with_absolute_axis(&setup)
                .expect("Failed to set abs axis");
        }
    }

    if let Some(misc) = source.misc_properties() {
        builder = builder.with_msc(&misc).expect("Failed to set msc");
    }

    builder.build().expect("Failed to build virtual device")
}

struct ScrollAccumulator {
    accumulator: i32,
    threshold: i32,
}

impl ScrollAccumulator {
    fn new(threshold: i32) -> Self {
        Self {
            accumulator: 0,
            threshold,
        }
    }

    /// Accumulate a hi-res scroll value. Returns the number of notches to emit
    /// (positive = scroll up, negative = scroll down, zero = not yet at threshold).
    fn accumulate(&mut self, value: i32) -> i32 {
        // Reset on direction change to avoid dead zone
        if (self.accumulator > 0 && value < 0) || (self.accumulator < 0 && value > 0) {
            self.accumulator = 0;
        }

        self.accumulator += value;

        let notches = self.accumulator / self.threshold;
        if notches != 0 {
            self.accumulator -= notches * self.threshold;
        }
        notches
    }
}

fn main() {
    let config = load_config();

    let mut device = match find_device(&config.device) {
        Some(d) => d,
        None => {
            eprintln!("No device found matching '{}'", config.device);
            process::exit(1);
        }
    };

    let dev_name = device.name().unwrap_or("unknown").to_string();
    eprintln!("Attached to: {dev_name}");
    eprintln!("Scroll threshold: {} hi-res ticks per notch", config.threshold);

    let mut virt = build_virtual_device(&device);
    eprintln!(
        "Virtual device: {}",
        virt.get_syspath()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );

    device.grab().expect("Failed to grab device (need root?)");
    eprintln!("Device grabbed. Filtering scroll events...");

    let mut acc = ScrollAccumulator::new(config.threshold);

    loop {
        match device.fetch_events() {
            Ok(events) => {
                let events: Vec<InputEvent> = events.collect();
                let mut filtered: Vec<InputEvent> = Vec::with_capacity(events.len());

                for event in &events {
                    if event.event_type() == EventType::RELATIVE {
                        let code = RelativeAxisType(event.code());
                        if code == RelativeAxisType::REL_WHEEL
                            || code == RelativeAxisType::REL_WHEEL_HI_RES
                        {
                            // Only process hi-res events through the accumulator;
                            // drop the paired REL_WHEEL events entirely since we
                            // synthesize our own.
                            if code == RelativeAxisType::REL_WHEEL {
                                continue;
                            }
                            let notches = acc.accumulate(event.value());
                            if notches != 0 {
                                // Emit both REL_WHEEL and REL_WHEEL_HI_RES so all
                                // apps see the same quantized scroll.
                                filtered.push(InputEvent::new(
                                    EventType::RELATIVE,
                                    RelativeAxisType::REL_WHEEL_HI_RES.0,
                                    notches * 120,
                                ));
                                filtered.push(InputEvent::new(
                                    EventType::RELATIVE,
                                    RelativeAxisType::REL_WHEEL.0,
                                    notches,
                                ));
                            }
                            continue;
                        }
                    }
                    filtered.push(*event);
                }

                if !filtered.is_empty() {
                    if let Err(e) = virt.emit(&filtered) {
                        eprintln!("Failed to emit events: {e}");
                    }
                }
            }
            Err(e) => {
                eprintln!("Error reading events: {e}");
                break;
            }
        }
    }
}
