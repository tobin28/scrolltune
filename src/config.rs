use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{fs, io};
use std::{process, time::{SystemTime, UNIX_EPOCH}};

pub(crate) const CONFIG_PATHS: &[&str] = &["/etc/scrolltune.toml", "scrolltune.toml"];

#[derive(Deserialize)]
pub struct Config {
    #[serde(default = "default_device")]
    pub device: String,
    #[serde(default = "default_threshold")]
    pub threshold: i32,
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

pub fn load_config() -> Result<Config, String> {
    load_config_internal(true)
}

pub fn load_config_quiet() -> Result<Config, String> {
    load_config_internal(false)
}

fn load_config_internal(log: bool) -> Result<Config, String> {
    for path in CONFIG_PATHS {
        if Path::new(path).exists() {
            let contents =
                fs::read_to_string(path).map_err(|e| format!("Failed to read {path}: {e}"))?;
            let config: Config =
                toml::from_str(&contents).map_err(|e| format!("Failed to parse {path}: {e}"))?;
            if log {
                eprintln!("Config loaded from: {path}");
            }
            return Ok(config);
        }
    }

    if log {
        eprintln!("No config file found, using defaults");
    }
    Ok(Config::default())
}

pub fn config_path_for_update() -> PathBuf {
    CONFIG_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("scrolltune.toml"))
}

pub fn update_device_setting(path: &Path, device_name: &str) -> Result<(), String> {
    update_setting(path, "device", toml::Value::String(device_name.to_string()).to_string())
}

pub fn update_threshold_setting(path: &Path, threshold: i32) -> Result<(), String> {
    update_setting(path, "threshold", threshold.to_string())
}

fn update_setting(path: &Path, key: &str, replacement_value: String) -> Result<(), String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Failed to read {}: {e}", path.display())),
    };

    let replacement = format!("{key} = {replacement_value}");

    let mut found = false;
    let mut updated_lines = Vec::new();

    for line in contents.lines() {
        if is_assignment(line, key) {
            let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
            let indent = &line[..indent_len];
            updated_lines.push(format!("{indent}{replacement}"));
            found = true;
        } else {
            updated_lines.push(line.to_string());
        }
    }

    if !found {
        if !updated_lines.is_empty() && !updated_lines.last().unwrap().is_empty() {
            updated_lines.push(String::new());
        }
        updated_lines.push(replacement);
    }

    let mut updated = updated_lines.join("\n");
    if !updated.ends_with('\n') {
        updated.push('\n');
    }

    atomic_write(path, updated.as_bytes())
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("scrolltune.toml");
    let temp_path = temp_path_for(parent, file_name);

    let mut temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| format!("Failed to create temporary file {}: {e}", temp_path.display()))?;

    if let Ok(metadata) = fs::metadata(path) {
        fs::set_permissions(&temp_path, metadata.permissions()).map_err(|e| {
            let _ = fs::remove_file(&temp_path);
            format!(
                "Failed to copy permissions to temporary file {}: {e}",
                temp_path.display()
            )
        })?;
    }

    if let Err(e) = temp_file.write_all(contents) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to write temporary file {}: {e}",
            temp_path.display()
        ));
    }

    if let Err(e) = temp_file.sync_all() {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to sync temporary file {}: {e}",
            temp_path.display()
        ));
    }

    drop(temp_file);

    if let Err(e) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "Failed to replace {} with {}: {e}",
            path.display(),
            temp_path.display()
        ));
    }

    // Best-effort parent directory sync so the rename is durable too.
    if let Ok(parent_dir) = fs::File::open(parent) {
        let _ = parent_dir.sync_all();
    }

    Ok(())
}

fn temp_path_for(parent: &Path, file_name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(
        ".{file_name}.tmp-{}-{nanos}",
        process::id(),
    ))
}

fn is_assignment(line: &str, expected_key: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false;
    }

    let Some((key, _)) = trimmed.split_once('=') else {
        return false;
    };

    key.trim() == expected_key
}

#[cfg(test)]
mod tests {
    use super::{is_assignment, update_device_setting, update_threshold_setting};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_device_assignment_lines() {
        assert!(is_assignment(r#"device = "foo""#, "device"));
        assert!(is_assignment("  device= \"foo\"", "device"));
        assert!(!is_assignment("# device = \"foo\"", "device"));
        assert!(!is_assignment("threshold = 120", "device"));
    }

    #[test]
    fn detects_threshold_assignment_lines() {
        assert!(is_assignment("threshold = 120", "threshold"));
        assert!(is_assignment("  threshold=60", "threshold"));
        assert!(!is_assignment("# threshold = 120", "threshold"));
        assert!(!is_assignment(r#"device = "foo""#, "threshold"));
    }

    #[test]
    fn updates_existing_device_line_and_preserves_comments() {
        let path = temp_path("update-existing");
        fs::write(
            &path,
            "# comment\n\ndevice = \"old\"\nthreshold = 120\n",
        )
        .unwrap();

        update_device_setting(&path, "New Mouse").unwrap();
        let updated = fs::read_to_string(&path).unwrap();

        assert_eq!(
            updated,
            "# comment\n\ndevice = \"New Mouse\"\nthreshold = 120\n"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn appends_device_line_when_missing() {
        let path = temp_path("append-device");
        fs::write(&path, "# comment\nthreshold = 120\n").unwrap();

        update_device_setting(&path, "New Mouse").unwrap();
        let updated = fs::read_to_string(&path).unwrap();

        assert_eq!(
            updated,
            "# comment\nthreshold = 120\n\ndevice = \"New Mouse\"\n"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn updates_existing_threshold_line_and_preserves_comments() {
        let path = temp_path("update-threshold");
        fs::write(
            &path,
            "# comment\n\ndevice = \"Mouse\"\nthreshold = 120\n",
        )
        .unwrap();

        update_threshold_setting(&path, 60).unwrap();
        let updated = fs::read_to_string(&path).unwrap();

        assert_eq!(
            updated,
            "# comment\n\ndevice = \"Mouse\"\nthreshold = 60\n"
        );

        let _ = fs::remove_file(path);
    }

    fn temp_path(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("scrolltune-{prefix}-{nanos}.toml"))
    }
}
