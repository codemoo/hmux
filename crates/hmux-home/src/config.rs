//! Read-only Home configuration compatibility. No setup, persistence or service actions.

use hmux_model::{Inventory, Profile, SCHEMA_VERSION};
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

const HOME_MAX_BYTES: u64 = 1024 * 1024;
const INVENTORY_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HomeConfig {
    pub schema_version: i64,
    pub role: String,
    pub inventory_path: PathBuf,
    pub state_dir: PathBuf,
}

impl HomeConfig {
    pub fn default_for(home: &Path) -> Self {
        let inventory_path = home.join(".config/hmux/inventory.toml");
        let state_dir = home.join(".local/state/hmux");
        Self {
            schema_version: SCHEMA_VERSION,
            role: "home".into(),
            inventory_path: if inventory_path.is_absolute() {
                clean_absolute_path(&inventory_path)
            } else {
                inventory_path
            },
            state_dir: if state_dir.is_absolute() {
                clean_absolute_path(&state_dir)
            } else {
                state_dir
            },
        }
    }
}

// Retired values are decoded to check their types and fields, then discarded.
#[allow(dead_code)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HomeWire {
    schema_version: Option<i64>,
    role: Option<String>,
    inventory_path: Option<String>,
    state_dir: Option<String>,
    client_id: Option<String>,
    dmz_alias: Option<String>,
    home_alias: Option<String>,
    agent_path: Option<String>,
    control_path: Option<String>,
    cache_dir: Option<String>,
    public_key_path: Option<String>,
    timeout_seconds: Option<i64>,
    update_check: Option<bool>,
}

#[allow(dead_code)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct InventoryWire {
    schema_version: i64,
    revision: String,
    profiles: Option<Vec<Profile>>,
    clients: Option<Vec<LegacyClient>>,
    identity_refs: Option<Vec<LegacyIdentityRef>>,
    hosts: Option<Vec<LegacyHost>>,
}

#[allow(dead_code)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyClient {
    id: String,
    role: String,
    hostnames: Option<Vec<String>>,
}

#[allow(dead_code)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyIdentityRef {
    id: String,
    path: String,
}

#[allow(dead_code)]
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LegacyHost {
    id: String,
    ssh_alias: String,
    address: String,
    user: String,
    port: i64,
    proxy_jump: String,
    identity_ref: String,
    tags: Option<Vec<String>>,
    server_alive_interval: i64,
    server_alive_count_max: i64,
    connect_timeout: i64,
}

pub fn load_home_current(path: Option<&Path>) -> Result<HomeConfig, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is unavailable")?;
    load_home(path, Path::new(&home))
}

pub fn load_home(path: Option<&Path>, home: &Path) -> Result<HomeConfig, String> {
    load_home_inner(path, home, true)
}

/// Explicit connector startup must not fall back to defaults if its selected
/// file is missing or disappears while opening. Shared decode callers retain
/// the existing optional-file contract through `load_home`.
pub fn load_home_required(path: &Path, home: &Path) -> Result<HomeConfig, String> {
    load_home_inner(Some(path), home, false)
}

fn load_home_inner(path: Option<&Path>, home: &Path, optional: bool) -> Result<HomeConfig, String> {
    if !home.is_absolute() {
        return Err("Home directory must be absolute".into());
    }
    let home = clean_absolute_path(home);
    let home = home.as_path();
    let mut config = HomeConfig::default_for(home);
    let selected = if let Some(path) = path {
        path.to_path_buf()
    } else {
        let current = home.join(".config/hmux/home.toml");
        match fs::symlink_metadata(&current) {
            Ok(_) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                home.join(".config/hmux/client.toml")
            }
            Err(error) => return Err(format!("inspect {}: {error}", current.display())),
        }
    };
    let Some(raw) = read_config(&selected, HOME_MAX_BYTES, optional)? else {
        return Ok(config);
    };
    let wire: HomeWire = toml::from_str(&raw).map_err(|e| format!("decode Home config: {e}"))?;
    // Accessing every retired field above ensures their types and shape are
    // checked by serde, but none of them influence execution or paths.
    if let Some(value) = wire.schema_version {
        config.schema_version = value;
    }
    if let Some(value) = wire.role {
        config.role = value;
    }
    if config.schema_version != SCHEMA_VERSION {
        return Err("Home schema_version must be 1".into());
    }
    if config.role != "home" {
        return Err("HMux supports web/PWA clients only; role must be home".into());
    }
    if let Some(value) = wire.inventory_path {
        config.inventory_path = PathBuf::from(value);
    }
    if let Some(value) = wire.state_dir {
        config.state_dir = PathBuf::from(value);
    }
    config.inventory_path = clean_user_path(&config.inventory_path, home)?;
    config.state_dir = clean_user_path(&config.state_dir, home)?;
    Ok(config)
}

pub fn load_inventory(path: &Path) -> Result<Inventory, String> {
    let raw =
        read_config(path, INVENTORY_MAX_BYTES, false)?.ok_or("inventory file does not exist")?;
    let wire: InventoryWire = toml::from_str(&raw).map_err(|e| format!("decode inventory: {e}"))?;
    let inventory = Inventory {
        schema_version: wire.schema_version,
        revision: wire.revision,
        profiles: wire.profiles,
    };
    inventory
        .validate()
        .map_err(|e| format!("validate inventory: {e}"))?;
    Ok(inventory)
}

fn read_config(path: &Path, maximum: u64, optional: bool) -> Result<Option<String>, String> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    validate_metadata(&before, maximum)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let after = file
        .metadata()
        .map_err(|e| format!("stat {}: {e}", path.display()))?;
    validate_metadata(&after, maximum)?;
    if before.dev() != after.dev() || before.ino() != after.ino() {
        return Err("configuration file changed while opening".into());
    }
    let mut bytes = Vec::with_capacity(after.len() as usize);
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read configuration: {e}"))?;
    if bytes.is_empty() || bytes.len() as u64 > maximum {
        return Err("configuration file size is invalid".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "configuration is not UTF-8".into())
}

fn validate_metadata(metadata: &fs::Metadata, maximum: u64) -> Result<(), String> {
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("configuration must be a regular non-symlink file".into());
    }
    if metadata.len() < 1 || metadata.len() > maximum {
        return Err("configuration file size is invalid".into());
    }
    if metadata.permissions().mode() & 0o022 != 0 {
        return Err("configuration must not be group/world writable".into());
    }
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err("configuration must be owned by the current user".into());
    }
    Ok(())
}

fn clean_user_path(path: &Path, home: &Path) -> Result<PathBuf, String> {
    let expanded = if path == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(suffix) = path.strip_prefix("~") {
        home.join(suffix)
    } else {
        path.to_path_buf()
    };
    if !expanded.is_absolute() {
        return Err("Home paths must be absolute user paths".into());
    }
    let cleaned = clean_absolute_path(&expanded);
    if cleaned == Path::new("/") {
        return Err("Home paths cannot be the filesystem root".into());
    }
    Ok(cleaned)
}

fn clean_absolute_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::from("/");
    for part in path.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::Normal(value) => cleaned.push(value),
            Component::Prefix(_) => {}
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_clean_and_defaults() {
        let home = Path::new("/synthetic/home");
        let config = HomeConfig::default_for(home);
        assert_eq!(
            config.inventory_path,
            home.join(".config/hmux/inventory.toml")
        );
        assert_eq!(
            HomeConfig::default_for(Path::new("/synthetic/home/../user")).inventory_path,
            Path::new("/synthetic/user/.config/hmux/inventory.toml")
        );
        assert_eq!(
            clean_user_path(Path::new("~/work/../state"), home).unwrap(),
            home.join("state")
        );
        assert!(clean_user_path(Path::new("/"), home).is_err());
        assert!(clean_user_path(Path::new("relative"), home).is_err());
    }
}
