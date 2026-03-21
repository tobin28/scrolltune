mod config;
mod devices;

use crate::config::{
    config_path_for_update, load_config, load_config_quiet, update_device_setting,
    update_threshold_setting,
};
use crate::devices::{
    find_device, list_mouse_like_devices, load_last_device, save_last_device, DeviceFingerprint,
    MouseDeviceInfo,
};
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{AbsInfo, AbsoluteAxisType, Device, EventType, InputEvent, RelativeAxisType};
use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::{self, Command as ProcessCommand};

const NO_TARGET_EXIT_CODE: i32 = 2;

/// scrolltune — scroll wheel filter for mice on Linux.
///
/// Grabs the mouse's evdev device, accumulates hi-res scroll ticks,
/// and only emits scroll events when a full notch threshold is reached,
/// preventing jittery scrolling from high-resolution scroll wheels.

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

enum Command {
    Run,
    Help,
    Status,
    ListDevices,
    SetDevice,
    SetThreshold(Option<String>),
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
    match parse_command() {
        Ok(Command::Help) => {
            print_help();
            return;
        }
        Ok(Command::Status) => {
            print_status();
            return;
        }
        Ok(Command::ListDevices) => {
            let devices = list_mouse_like_devices();
            if devices.is_empty() {
                eprintln!("No mouse-like devices found");
                process::exit(NO_TARGET_EXIT_CODE);
            }
            print_device_details(&devices);
            return;
        }
        Ok(Command::SetDevice) => {
            let devices = list_mouse_like_devices();
            if devices.is_empty() {
                eprintln!("No mouse-like devices found");
                process::exit(NO_TARGET_EXIT_CODE);
            }
            if let Err(e) = run_set_device(&devices) {
                eprintln!("{e}");
                process::exit(1);
            }
            return;
        }
        Ok(Command::SetThreshold(value)) => {
            if let Err(e) = run_set_threshold(value) {
                eprintln!("{e}");
                process::exit(1);
            }
            return;
        }
        Ok(Command::Run) => {}
        Err(e) => {
            eprintln!("{e}");
            eprintln!();
            print_help();
            process::exit(1);
        }
    }

    let config = match load_config() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    };

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

fn parse_command() -> Result<Command, String> {
    let args: Vec<_> = env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(Command::Run),
        [flag] if flag == "--help" || flag == "-h" => Ok(Command::Help),
        [flag] if flag == "--status" || flag == "-s" => Ok(Command::Status),
        [flag] if flag == "--list-devices" || flag == "-l" => Ok(Command::ListDevices),
        [flag] if flag == "--set-device" || flag == "-d" => Ok(Command::SetDevice),
        [flag] if flag == "--set-threshold" || flag == "-t" => Ok(Command::SetThreshold(None)),
        [flag, value] if flag == "--set-threshold" || flag == "-t" => {
            Ok(Command::SetThreshold(Some(value.clone())))
        }
        _ => Err("Unknown arguments".to_string()),
    }
}

fn print_help() {
    println!("scrolltune");
    println!("Scroll wheel filter for hi-res mouse wheels on Linux.\n");
    println!("Usage:");
    println!("  scrolltune");
    println!("  scrolltune --help | -h");
    println!("  scrolltune --status | -s");
    println!("  scrolltune --list-devices | -l");
    println!("  scrolltune --set-device | -d");
    println!("  scrolltune --set-threshold | -t [VALUE]");
    println!();
    println!("Commands:");
    println!("  --help, -h");
    println!("      Show this help text.");
    println!("  --status, -s");
    println!("      Show service state, last attached device, and configured threshold.");
    println!("  --list-devices, -l");
    println!("      List all mouse-like evdev devices with diagnostic details.");
    println!("  --set-device, -d");
    println!("      Interactively choose a selectable mouse device and update the config.");
    println!("  --set-threshold, -t [VALUE]");
    println!("      Set the hi-res scroll threshold interactively or directly.");
    println!();
    println!("Examples:");
    println!("  scrolltune -s");
    println!("  scrolltune -l");
    println!("  scrolltune -d");
    println!("  scrolltune -t");
    println!("  scrolltune -t 60");
    println!();
    println!("Note:");
    println!("  Use sudo for commands that update /etc/scrolltune.toml or restart");
    println!("  scrolltune.service. Listing devices does not require sudo.");
}

fn print_status() {
    println!("service: {}", service_state_label());

    match load_last_device() {
        Some(fingerprint) => println!("last attached device: {}", fingerprint.name()),
        None => println!("last attached device: (none)"),
    }

    match load_config_quiet() {
        Ok(config) => {
            if config.device.trim().is_empty() {
                println!("configured device filter: (auto)");
            } else {
                println!("configured device filter: {}", config.device);
            }
            println!("configured threshold: {}", config.threshold);
        }
        Err(e) => {
            println!("configured device filter: (error)");
            println!("configured threshold: (error)");
            println!("config error: {e}");
        }
    }
}

fn service_state_label() -> String {
    let output = match ProcessCommand::new("systemctl")
        .args(["is-active", "scrolltune.service"])
        .output()
    {
        Ok(output) => output,
        Err(e) => return format!("unknown ({e})"),
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if !stdout.is_empty() {
        return stdout;
    }

    if !stderr.is_empty() {
        return format!("unknown ({stderr})");
    }

    format!("unknown (exit status {})", output.status)
}

fn print_device_details(devices: &[MouseDeviceInfo]) {
    for device in devices {
        println!("{}", device.path.display());
        println!("  name: {}", device.name);
        println!("  bus: {:04x}", device.bus_type);
        println!("  vendor: {:04x}", device.vendor);
        println!("  product: {:04x}", device.product);
        println!("  version: {:04x}", device.version);
        println!(
            "  physical path: {}",
            device.physical_path.as_deref().unwrap_or("")
        );
        println!(
            "  unique name: {}",
            device.unique_name.as_deref().unwrap_or("")
        );
        println!("  virtual: {}", yes_no(device.is_virtual));
        println!("  hi-res wheel: {}", yes_no(device.has_hi_res_wheel));
        println!(
            "  candidate: {}",
            yes_no(device.candidate_status.is_selectable())
        );
        if let Some(reason) = device.candidate_reason() {
            println!("  reason: {reason}");
        }
        println!();
    }
}

fn run_set_device(devices: &[MouseDeviceInfo]) -> Result<(), String> {
    let selectable_devices: Vec<_> = devices
        .iter()
        .filter(|device| device.candidate_status.is_selectable())
        .collect();

    if selectable_devices.is_empty() {
        return Err("No selectable mouse devices found".to_string());
    }

    let current_selection = load_last_device();
    print_device_picker(&selectable_devices, current_selection.as_ref());
    print!("Select device by number or exact name.\nPress Enter to cancel.\n> ");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read selection: {e}"))?;
    let selection = input.trim();

    if selection.is_empty() || matches!(selection, "q" | "quit") {
        println!("No changes made.");
        return Ok(());
    }

    let selected = if let Ok(index) = selection.parse::<usize>() {
        if index == 0 || index > selectable_devices.len() {
            return Err(format!("Invalid selection '{selection}'"));
        }
        selectable_devices[index - 1]
    } else {
        let matches: Vec<_> = selectable_devices
            .iter()
            .copied()
            .filter(|device| device.name == selection || display_device_name(&device.name) == selection)
            .collect();
        match matches.len() {
            0 => return Err(format!("No device found with exact name '{selection}'")),
            1 => matches[0],
            _ => {
                return Err(format!(
                    "Multiple devices match '{selection}'. Select by number instead."
                ));
            }
        }
    };

    let path = config_path_for_update();
    update_device_setting(&path, &selected.name)?;
    println!("Updated {}", path.display());
    println!(
        "device = {}",
        toml::Value::String(selected.name.clone())
    );

    match restart_service_if_needed(&path) {
        Ok(Some(message)) => println!("{message}"),
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

fn run_set_threshold(value: Option<String>) -> Result<(), String> {
    let threshold = match value {
        Some(value) => parse_threshold(&value)?,
        None => {
            let config = load_config()?;
            match prompt_for_threshold(config.threshold)? {
                Some(threshold) => threshold,
                None => {
                    println!("No changes made.");
                    return Ok(());
                }
            }
        }
    };

    let path = config_path_for_update();
    update_threshold_setting(&path, threshold)?;
    println!("Updated {}", path.display());
    println!("threshold = {threshold}");

    match restart_service_if_needed(&path) {
        Ok(Some(message)) => println!("{message}"),
        Ok(None) => {}
        Err(e) => return Err(e),
    }

    Ok(())
}

fn print_device_picker(devices: &[&MouseDeviceInfo], current_selection: Option<&DeviceFingerprint>) {
    println!("\nMouse-like devices\n");

    let marker_width = 1;
    let index_width = devices.len().to_string().len().max(1);
    let name_width = devices
        .iter()
        .map(|device| display_device_name(&device.name).len())
        .max()
        .unwrap_or(4)
        .max(4);
    let event_width = devices
        .iter()
        .map(|device| device.path.display().to_string().len())
        .max()
        .unwrap_or(5)
        .max(5);
    let status_width = devices
        .iter()
        .map(|device| device.candidate_status.label().len())
        .max()
        .unwrap_or(6)
        .max(6);

    let border = format!(
        "+-{:-<marker_width$}-+-{:-<index_width$}-+-{:-<name_width$}-+-{:-<event_width$}-+-{:-<status_width$}-+",
        "",
        "",
        "",
        "",
        "",
        marker_width = marker_width,
        index_width = index_width,
        name_width = name_width,
        event_width = event_width,
        status_width = status_width,
    );
    println!("{border}");
    println!(
        "| {:<marker_width$} | {:<index_width$} | {:<name_width$} | {:<event_width$} | {:<status_width$} |",
        "*",
        "#",
        "Name",
        "Event",
        "Status",
        marker_width = marker_width,
        index_width = index_width,
        name_width = name_width,
        event_width = event_width,
        status_width = status_width,
    );
    println!("{border}");

    for (index, device) in devices.iter().enumerate() {
        let marker = if current_selection
            .map(|fingerprint| fingerprint.matches_info(device))
            .unwrap_or(false)
        {
            "*"
        } else {
            " "
        };
        println!(
            "| {:<marker_width$} | {:>index_width$} | {:<name_width$} | {:<event_width$} | {:<status_width$} |",
            marker,
            index + 1,
            display_device_name(&device.name),
            device.path.display(),
            device.candidate_status.label(),
            marker_width = marker_width,
            index_width = index_width,
            name_width = name_width,
            event_width = event_width,
            status_width = status_width,
        );
    }

    println!("{border}");
    if let Some(current_selection) = current_selection {
        if devices
            .iter()
            .any(|device| current_selection.matches_info(device))
        {
            println!("* current running service device");
        } else {
            println!(
                "Current running service device: {} (not shown in selectable list)",
                current_selection.name()
            );
        }
    }
    println!();
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn display_device_name(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.is_empty() { name } else { trimmed }
}

fn prompt_for_threshold(current_threshold: i32) -> Result<Option<i32>, String> {
    println!("\nCurrent threshold: {current_threshold}\n");
    print!("Enter new threshold (positive integer).\nPress Enter to cancel.\n> ");
    io::stdout()
        .flush()
        .map_err(|e| format!("Failed to flush stdout: {e}"))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("Failed to read threshold: {e}"))?;
    let selection = input.trim();

    if selection.is_empty() || matches!(selection, "q" | "quit") {
        return Ok(None);
    }

    parse_threshold(selection).map(Some)
}

fn parse_threshold(value: &str) -> Result<i32, String> {
    let threshold = value
        .parse::<i32>()
        .map_err(|_| format!("Invalid threshold '{value}': must be a positive integer"))?;
    if threshold <= 0 {
        return Err(format!(
            "Invalid threshold '{value}': must be a positive integer"
        ));
    }
    Ok(threshold)
}

fn restart_service_if_needed(config_path: &Path) -> Result<Option<String>, String> {
    if config_path != Path::new("/etc/scrolltune.toml") {
        return Ok(Some(
            "Updated local config only; systemd service restart skipped.".to_string(),
        ));
    }

    let output = ProcessCommand::new("systemctl")
        .args(["restart", "scrolltune.service"])
        .output()
        .map_err(|e| format!("Updated config, but failed to restart scrolltune.service: {e}"))?;

    if output.status.success() {
        Ok(Some("Restarted scrolltune.service".to_string()))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exit status {}", output.status)
        } else {
            stderr
        };
        Err(format!(
            "Updated config, but failed to restart scrolltune.service: {detail}"
        ))
    }
}
