use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AbsInfo, AbsoluteAxisType, Device, EventType, InputEvent, RelativeAxisType};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::{fs, process};

const CONFIG_PATHS: &[&str] = &["/etc/scrolltune.toml", "scrolltune.toml"];
const LAST_DEVICE_PATH: &str = "/var/lib/scrolltune/last-device.toml";
const NO_TARGET_EXIT_CODE: i32 = 2;

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

fn default_device() -> String {
    String::new()
}
fn default_threshold() -> i32 {
    120
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: default_device(),
            threshold: default_threshold(),
        }
    }
}

#[derive(Deserialize, Serialize)]
struct DeviceFingerprint {
    name: String,
    bus_type: u16,
    vendor: u16,
    product: u16,
    version: u16,
    physical_path: Option<String>,
    unique_name: Option<String>,
}

type EnumeratedDevice = (PathBuf, Device);

impl DeviceFingerprint {
    fn from_device(dev: &Device) -> Self {
        let input_id = dev.input_id();
        Self {
            name: dev.name().unwrap_or("unknown").to_string(),
            bus_type: input_id.bus_type().0,
            vendor: input_id.vendor(),
            product: input_id.product(),
            version: input_id.version(),
            physical_path: dev.physical_path().map(ToOwned::to_owned),
            unique_name: dev.unique_name().map(ToOwned::to_owned),
        }
    }

    fn matches_name_and_id(&self, dev: &Device) -> bool {
        let input_id = dev.input_id();
        self.name == dev.name().unwrap_or("unknown")
            && self.bus_type == input_id.bus_type().0
            && self.vendor == input_id.vendor()
            && self.product == input_id.product()
            && self.version == input_id.version()
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

fn load_last_device() -> Option<DeviceFingerprint> {
    let path = Path::new(LAST_DEVICE_PATH);
    if !path.exists() {
        return None;
    }

    match fs::read_to_string(path) {
        Ok(contents) => match toml::from_str(&contents) {
            Ok(fingerprint) => Some(fingerprint),
            Err(e) => {
                eprintln!("Failed to parse {LAST_DEVICE_PATH}: {e}");
                None
            }
        },
        Err(e) => {
            eprintln!("Failed to read {LAST_DEVICE_PATH}: {e}");
            None
        }
    }
}

fn save_last_device(fingerprint: &DeviceFingerprint) {
    let path = Path::new(LAST_DEVICE_PATH);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("Failed to create {}: {e}", parent.display());
            return;
        }
    }

    match toml::to_string_pretty(fingerprint) {
        Ok(contents) => {
            if let Err(e) = fs::write(path, contents) {
                eprintln!("Failed to write {LAST_DEVICE_PATH}: {e}");
            }
        }
        Err(e) => eprintln!("Failed to serialize last device state: {e}"),
    }
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

fn supports_hi_res_scroll(dev: &Device) -> bool {
    dev.supported_relative_axes()
        .map(|axes| axes.contains(RelativeAxisType::REL_WHEEL_HI_RES))
        .unwrap_or(false)
}

/// Known mouse vendor IDs for auto-detection when multiple mice are found.
const KNOWN_MOUSE_VENDORS: &[(u16, &str)] = &[(0x046d, "Logitech")];

fn is_known_mouse_vendor(dev: &Device) -> bool {
    let vendor = dev.input_id().vendor();
    KNOWN_MOUSE_VENDORS.iter().any(|(id, _)| *id == vendor)
}

fn take_device_by_path(mice: Vec<EnumeratedDevice>, path: &Path) -> Device {
    mice.into_iter()
        .find(|(p, _)| p == path)
        .map(|(_, dev)| dev)
        .unwrap()
}

fn find_last_known_device(
    supported: &[&EnumeratedDevice],
    fingerprint: &DeviceFingerprint,
) -> Option<PathBuf> {
    let base_matches: Vec<_> = supported
        .iter()
        .copied()
        .filter(|(_, dev)| fingerprint.matches_name_and_id(dev))
        .collect();

    if base_matches.len() == 1 {
        return Some(base_matches[0].0.clone());
    }

    if let Some(unique_name) = fingerprint.unique_name.as_deref() {
        let unique_matches: Vec<_> = base_matches
            .iter()
            .copied()
            .filter(|(_, dev)| dev.unique_name() == Some(unique_name))
            .collect();
        if unique_matches.len() == 1 {
            return Some(unique_matches[0].0.clone());
        }
    }

    if let Some(physical_path) = fingerprint.physical_path.as_deref() {
        let physical_matches: Vec<_> = base_matches
            .iter()
            .copied()
            .filter(|(_, dev)| dev.physical_path() == Some(physical_path))
            .collect();
        if physical_matches.len() == 1 {
            return Some(physical_matches[0].0.clone());
        }
    }

    None
}

fn find_device(name_filter: &str) -> Result<Device, String> {
    let mice: Vec<_> = evdev::enumerate()
        .filter(|(_, dev)| is_mouse(dev))
        .collect();

    if mice.is_empty() {
        return Err("No mouse devices found".to_string());
    }

    // Priority 1: explicit name filter from config
    if !name_filter.is_empty() {
        let path = mice
            .iter()
            .find(|(_, dev)| {
                dev.name()
                    .map(|n| n.to_lowercase().contains(&name_filter.to_lowercase()))
                    .unwrap_or(false)
            })
            .map(|(path, _)| path.clone())
            .ok_or_else(|| format!("No mouse found matching '{name_filter}'"))?;

        let dev = take_device_by_path(mice, &path);
        if supports_hi_res_scroll(&dev) {
            return Ok(dev);
        }

        return Err(format!(
            "Device '{}' does not support REL_WHEEL_HI_RES",
            dev.name().unwrap_or("unknown")
        ));
    }

    let supported: Vec<_> = mice
        .iter()
        .filter(|(_, dev)| supports_hi_res_scroll(dev))
        .collect();

    if supported.is_empty() {
        eprintln!("No mice with REL_WHEEL_HI_RES support found");
        eprintln!("Mouse candidates:");
        for (_, dev) in &mice {
            eprintln!("  - {}", dev.name().unwrap_or("unknown"));
        }
        return Err("scrolltune only supports mice that expose REL_WHEEL_HI_RES".to_string());
    }

    // Priority 2: only one mouse detected
    if supported.len() == 1 {
        let path = supported[0].0.clone();
        let dev = take_device_by_path(mice, &path);
        eprintln!("Auto-detected: {}", dev.name().unwrap_or("unknown"));
        return Ok(dev);
    }

    // Priority 3: prefer the last known working device when multiple are present
    if let Some(fingerprint) = load_last_device() {
        if let Some(path) = find_last_known_device(&supported, &fingerprint) {
            let dev = take_device_by_path(mice, &path);
            eprintln!(
                "Auto-detected from last known device: {}",
                dev.name().unwrap_or("unknown")
            );
            return Ok(dev);
        }
    }

    // Priority 4: filter by known mouse vendors
    let known: Vec<_> = supported
        .iter()
        .filter(|(_, dev)| is_known_mouse_vendor(dev))
        .collect();

    if known.len() == 1 {
        let (path, _) = known[0];
        let path = path.clone();
        let dev = take_device_by_path(mice, &path);
        let vendor = dev.input_id().vendor();
        let vendor_name = KNOWN_MOUSE_VENDORS
            .iter()
            .find(|(id, _)| *id == vendor)
            .map(|(_, name)| *name)
            .unwrap_or("unknown");
        eprintln!(
            "Auto-detected by vendor ({}): {}",
            vendor_name,
            dev.name().unwrap_or("unknown")
        );
        return Ok(dev);
    }

    // Give up — print candidates to help the user configure
    eprintln!("Multiple supported mice found — set 'device' in config:");
    for (_, dev) in &supported {
        let vendor = dev.input_id().vendor();
        let product = dev.input_id().product();
        eprintln!(
            "  - {} (vendor: {:04x}, product: {:04x})",
            dev.name().unwrap_or("unknown"),
            vendor,
            product
        );
    }
    Err("Multiple supported mice found".to_string())
}

fn build_virtual_device(source: &Device) -> VirtualDevice {
    let name = format!("{} (scrolltune)", source.name().unwrap_or("mouse"));
    let mut builder = VirtualDeviceBuilder::new()
        .expect("Failed to create virtual device builder")
        .name(&name);

    if let Some(keys) = source.supported_keys() {
        builder = builder.with_keys(keys).expect("Failed to set keys");
    }

    if let Some(rel_axes) = source.supported_relative_axes() {
        builder = builder
            .with_relative_axes(rel_axes)
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
        builder = builder.with_msc(misc).expect("Failed to set msc");
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

    if config.threshold <= 0 {
        eprintln!(
            "Invalid threshold {}: threshold must be a positive integer",
            config.threshold
        );
        process::exit(1);
    }

    let mut device = match find_device(&config.device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            process::exit(NO_TARGET_EXIT_CODE);
        }
    };

    let dev_name = device.name().unwrap_or("unknown").to_string();
    eprintln!("Attached to: {dev_name}");
    eprintln!(
        "Scroll threshold: {} hi-res ticks per notch",
        config.threshold
    );

    let mut virt = build_virtual_device(&device);
    eprintln!(
        "Virtual device: {}",
        virt.get_syspath()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    );

    device.grab().expect("Failed to grab device (need root?)");
    eprintln!("Device grabbed. Filtering scroll events...");
    save_last_device(&DeviceFingerprint::from_device(&device));

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
                process::exit(1);
            }
        }
    }
}
