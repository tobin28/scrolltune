use evdev::{BusType, Device, EventType, RelativeAxisType};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::fs;

const LAST_DEVICE_PATH: &str = "/var/lib/scrolltune/last-device.toml";

/// Known mouse vendor IDs for auto-detection when multiple mice are found.
const KNOWN_MOUSE_VENDORS: &[(u16, &str)] = &[(0x046d, "Logitech")];

type EnumeratedDevice = (PathBuf, Device);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateStatus {
    Selectable,
    VirtualDevice,
    NoHiResWheel,
}

impl CandidateStatus {
    pub fn is_selectable(self) -> bool {
        matches!(self, Self::Selectable)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Selectable => "selectable",
            Self::VirtualDevice => "virtual device",
            Self::NoHiResWheel => "no hi-res wheel",
        }
    }
}

#[derive(Clone, Debug)]
pub struct MouseDeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub bus_type: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub physical_path: Option<String>,
    pub unique_name: Option<String>,
    pub is_virtual: bool,
    pub has_hi_res_wheel: bool,
    pub candidate_status: CandidateStatus,
}

impl MouseDeviceInfo {
    pub fn candidate_reason(&self) -> Option<&'static str> {
        match self.candidate_status {
            CandidateStatus::Selectable => None,
            status => Some(status.label()),
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct DeviceFingerprint {
    name: String,
    bus_type: u16,
    vendor: u16,
    product: u16,
    version: u16,
    physical_path: Option<String>,
    unique_name: Option<String>,
}

impl DeviceFingerprint {
    pub fn from_device(dev: &Device) -> Self {
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

    pub fn matches_info(&self, info: &MouseDeviceInfo) -> bool {
        if self.name != info.name
            || self.bus_type != info.bus_type
            || self.vendor != info.vendor
            || self.product != info.product
            || self.version != info.version
        {
            return false;
        }

        if let Some(unique_name) = self.unique_name.as_deref() {
            if info.unique_name.as_deref() != Some(unique_name) {
                return false;
            }
        }

        if let Some(physical_path) = self.physical_path.as_deref() {
            if info.physical_path.as_deref() != Some(physical_path) {
                return false;
            }
        }

        true
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub fn load_last_device() -> Option<DeviceFingerprint> {
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

pub fn save_last_device(fingerprint: &DeviceFingerprint) {
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

pub fn list_mouse_like_devices() -> Vec<MouseDeviceInfo> {
    evdev::enumerate()
        .filter(|(_, dev)| is_mouse_like(dev))
        .map(|(path, dev)| to_device_info(path, &dev))
        .collect()
}

pub fn find_device(name_filter: &str) -> Result<Device, String> {
    let mice = enumerate_mouse_like_devices();

    if mice.is_empty() {
        return Err("No mouse-like devices found".to_string());
    }

    if !name_filter.is_empty() {
        return find_device_by_name(mice, name_filter);
    }

    let supported: Vec<_> = mice
        .iter()
        .filter(|(path, dev)| candidate_status(path, dev).is_selectable())
        .collect();

    if supported.is_empty() {
        eprintln!("No mice with REL_WHEEL_HI_RES support found");
        eprintln!("Mouse-like devices:");
        for (_, dev) in &mice {
            eprintln!("  - {}", dev.name().unwrap_or("unknown"));
        }
        return Err("scrolltune only supports mice that expose REL_WHEEL_HI_RES".to_string());
    }

    if supported.len() == 1 {
        let path = supported[0].0.clone();
        let dev = take_device_by_path(mice, &path)?;
        eprintln!("Auto-detected: {}", dev.name().unwrap_or("unknown"));
        return Ok(dev);
    }

    if let Some(fingerprint) = load_last_device() {
        if let Some(path) = find_last_known_device(&supported, &fingerprint) {
            let dev = take_device_by_path(mice, &path)?;
            eprintln!(
                "Auto-detected from last known device: {}",
                dev.name().unwrap_or("unknown")
            );
            return Ok(dev);
        }
    }

    let known: Vec<_> = supported
        .iter()
        .copied()
        .filter(|(_, dev)| is_known_mouse_vendor(dev))
        .collect();

    if known.len() == 1 {
        let (path, dev_ref) = known[0];
        let path = path.clone();
        let vendor = dev_ref.input_id().vendor();
        let dev = take_device_by_path(mice, &path)?;
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

fn enumerate_mouse_like_devices() -> Vec<EnumeratedDevice> {
    evdev::enumerate()
        .filter(|(_, dev)| is_mouse_like(dev))
        .collect()
}

fn find_device_by_name(mice: Vec<EnumeratedDevice>, name_filter: &str) -> Result<Device, String> {
    let selectable_exact = mice
        .iter()
        .find(|(_, dev)| {
            dev.name() == Some(name_filter)
        })
        .and_then(|(path, dev)| {
            if candidate_status(path, dev).is_selectable() {
                Some(path.clone())
            } else {
                None
            }
        });
    if let Some(path) = selectable_exact {
        return take_device_by_path(mice, &path);
    }

    let lowered = name_filter.to_lowercase();
    let selectable_substring = mice
        .iter()
        .find(|(_, dev)| {
            dev.name()
                .map(|name| name.to_lowercase().contains(&lowered))
                .unwrap_or(false)
        })
        .and_then(|(path, dev)| {
            if candidate_status(path, dev).is_selectable() {
                Some(path.clone())
            } else {
                None
            }
        });
    if let Some(path) = selectable_substring {
        return take_device_by_path(mice, &path);
    }

    if let Some((path, dev)) = mice.iter().find(|(_, dev)| dev.name() == Some(name_filter)) {
        return Err(format_match_failure(path, dev));
    }

    if let Some((path, dev)) = mice.iter().find(|(_, dev)| {
        dev.name()
            .map(|name| name.to_lowercase().contains(&lowered))
            .unwrap_or(false)
    }) {
        return Err(format_match_failure(path, dev));
    }

    Err(format!("No mouse found matching '{name_filter}'"))
}

fn format_match_failure(path: &Path, dev: &Device) -> String {
    match candidate_status(path, dev) {
        CandidateStatus::Selectable => {
            format!("Device '{}' could not be selected", dev.name().unwrap_or("unknown"))
        }
        status => format!(
            "Device '{}' is not selectable: {}",
            dev.name().unwrap_or("unknown"),
            status.label()
        ),
    }
}

fn is_mouse_like(dev: &Device) -> bool {
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

fn is_virtual_device(path: &Path, dev: &Device) -> bool {
    if dev.input_id().bus_type() == BusType::BUS_VIRTUAL {
        return true;
    }

    let Some(event_name) = path.file_name() else {
        return false;
    };

    let sysfs_device_path = Path::new("/sys/class/input").join(event_name).join("device");
    fs::canonicalize(sysfs_device_path)
        .map(|resolved| resolved.starts_with("/sys/devices/virtual"))
        .unwrap_or(false)
}

fn supports_hi_res_scroll(dev: &Device) -> bool {
    dev.supported_relative_axes()
        .map(|axes| axes.contains(RelativeAxisType::REL_WHEEL_HI_RES))
        .unwrap_or(false)
}

fn candidate_status(path: &Path, dev: &Device) -> CandidateStatus {
    if is_virtual_device(path, dev) {
        CandidateStatus::VirtualDevice
    } else if !supports_hi_res_scroll(dev) {
        CandidateStatus::NoHiResWheel
    } else {
        CandidateStatus::Selectable
    }
}

fn to_device_info(path: PathBuf, dev: &Device) -> MouseDeviceInfo {
    let input_id = dev.input_id();
    let is_virtual = is_virtual_device(&path, dev);
    let candidate_status = candidate_status(&path, dev);
    MouseDeviceInfo {
        path,
        name: dev.name().unwrap_or("unknown").to_string(),
        bus_type: input_id.bus_type().0,
        vendor: input_id.vendor(),
        product: input_id.product(),
        version: input_id.version(),
        physical_path: dev.physical_path().map(ToOwned::to_owned),
        unique_name: dev.unique_name().map(ToOwned::to_owned),
        is_virtual,
        has_hi_res_wheel: supports_hi_res_scroll(dev),
        candidate_status,
    }
}

fn take_device_by_path(mice: Vec<EnumeratedDevice>, path: &Path) -> Result<Device, String> {
    mice.into_iter()
        .find(|(candidate_path, _)| candidate_path == path)
        .map(|(_, dev)| dev)
        .ok_or_else(|| format!("Internal error: device path {} disappeared", path.display()))
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

fn is_known_mouse_vendor(dev: &Device) -> bool {
    let vendor = dev.input_id().vendor();
    KNOWN_MOUSE_VENDORS.iter().any(|(id, _)| *id == vendor)
}
