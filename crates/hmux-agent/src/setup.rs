//! Conservative local setup. Existing config remains byte-for-byte stable unless
//! the administrator supplies a new workspace, in which case inventory is backed
//! up and only profile default_directory fields change.
use hmux_home::config;
use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("HOME is unavailable".into())
}
fn expand(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(s) = value.strip_prefix("~/") {
        home.join(s)
    } else {
        PathBuf::from(value)
    }
}
fn trusted(path: &Path) -> Result<()> {
    let uid = rustix::process::getuid().as_raw();
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(m)
                if m.is_dir()
                    && !m.file_type().is_symlink()
                    && (m.uid() == uid || m.uid() == 0)
                    && (m.permissions().mode() & 0o022 == 0
                        || m.uid() == 0 && m.permissions().mode() & 0o1000 != 0) => {}
            Ok(_) => {
                return Err(format!(
                    "untrusted configuration directory: {}",
                    ancestor.display()
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}
fn check_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m)
            if m.is_file()
                && !m.file_type().is_symlink()
                && m.uid() == rustix::process::getuid().as_raw()
                && m.permissions().mode() & 0o022 == 0 =>
        {
            Ok(true)
        }
        Ok(_) => Err(format!("unsafe configuration file: {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.to_string()),
    }
}
fn write_atomic(path: &Path, raw: &[u8]) -> Result<()> {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| e.to_string())?;
    let tmp = path.with_extension(format!("hmux-tmp-{:016x}", u64::from_le_bytes(random)));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        file.write_all(raw).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        sync_parent(path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or("configuration path has no parent")?;
    let dir = fs::File::open(parent).map_err(|e| e.to_string())?;
    dir.sync_all().map_err(|e| e.to_string())
}
fn backup(path: &Path) -> Result<()> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let raw = read(path, 16 << 20)?;
    for suffix in 0..100 {
        let dest = PathBuf::from(format!("{}.hmux-backup-{secs}-{suffix}", path.display()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&dest)
        {
            Ok(mut file) => {
                file.write_all(&raw).map_err(|e| e.to_string())?;
                file.sync_all().map_err(|e| e.to_string())?;
                drop(file);
                sync_parent(&dest)?;
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("cannot allocate configuration backup".into())
}
fn read(path: &Path, max: u64) -> Result<Vec<u8>> {
    check_file(path)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.is_empty() || raw.len() as u64 > max {
        return Err("invalid configuration size".into());
    }
    Ok(raw)
}
fn workspace(value: &str, home: &Path) -> Result<String> {
    let path = expand(value, home);
    if value.len() > 4096
        || value.chars().any(char::is_control)
        || !path.is_absolute()
        || path == Path::new("/")
    {
        return Err("workspace directory must be an absolute user path or start with ~/".into());
    }
    match fs::metadata(&path) {
        Ok(m) if !m.is_dir() => Err("workspace path is not a directory".into()),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(value.into()),
    }
}
fn new_inventory(directory: &str) -> String {
    let shell = env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("zsh"))
                .find(|p| {
                    fs::metadata(p)
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
        })
        .map_or("sh", |_| "zsh");
    format!("schema_version = 1\nrevision = \"installed\"\n\n[[profiles]]\nid = \"codex\"\nlabel = \"Codex\"\ndefault_directory = {dir}\ncommand = [\"codex\"]\ntags = [\"ai\", \"codex\"]\n\n[[profiles]]\nid = \"claude\"\nlabel = \"Claude Code\"\ndefault_directory = {dir}\ncommand = [\"claude\"]\ntags = [\"ai\", \"claude\"]\n\n[[profiles]]\nid = \"shell\"\nlabel = \"Shell\"\ndefault_directory = {dir}\ncommand = [\"{shell}\", \"-l\"]\ntags = [\"shell\"]\n",dir=toml::Value::String(directory.into()))
}
pub fn run(args: &[String]) -> Result<()> {
    let home = home()?;
    let mut directory = home.join(".config/hmux");
    let mut selected_workspace = String::new();
    let mut i = 0;
    while i < args.len() {
        let (flag, value, advance) = if let Some((flag, value)) = args[i].split_once('=') {
            (flag, value.to_owned(), 1)
        } else {
            (
                args[i].as_str(),
                args.get(i + 1)
                    .ok_or("unexpected setup-home arguments")?
                    .clone(),
                2,
            )
        };
        match flag {
            "--config-dir" => directory = expand(&value, &home),
            "--workspace-dir" => selected_workspace = value,
            _ => return Err("unexpected setup-home arguments".into()),
        }
        i += advance;
    }
    if !directory.is_absolute() {
        return Err("config directory must be absolute".into());
    }
    trusted(&directory)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    let home_path = directory.join("home.toml");
    let legacy_path = directory.join("client.toml");
    let existing = if check_file(&home_path)? {
        Some(home_path.clone())
    } else if check_file(&legacy_path)? {
        Some(legacy_path)
    } else {
        None
    };
    let cfg = if let Some(ref path) = existing {
        config::load_home_required(path, &home)?
    } else {
        let mut c = config::HomeConfig::default_for(&home);
        c.inventory_path = directory.join("inventory.toml");
        c
    };
    trusted(
        cfg.inventory_path
            .parent()
            .ok_or("invalid inventory path")?,
    )?;
    let new_inventory = !check_file(&cfg.inventory_path)?;
    if !new_inventory {
        config::load_inventory(&cfg.inventory_path)?;
    }
    if selected_workspace.is_empty() && new_inventory {
        selected_workspace = "~/.hmux".into();
    }
    if !selected_workspace.is_empty() {
        workspace(&selected_workspace, &home)?;
    }
    if new_inventory {
        fs::create_dir_all(
            cfg.inventory_path
                .parent()
                .ok_or("invalid inventory path")?,
        )
        .map_err(|e| e.to_string())?;
        write_atomic(
            &cfg.inventory_path,
            new_inventory_text(&selected_workspace).as_bytes(),
        )?;
    } else if !selected_workspace.is_empty() {
        let raw = read(&cfg.inventory_path, 16 << 20)?;
        let text = String::from_utf8(raw).map_err(|_| "inventory is not UTF-8")?;
        let mut value: toml::Value = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let profiles = value
            .get_mut("profiles")
            .and_then(toml::Value::as_array_mut)
            .ok_or("invalid inventory profile tables")?;
        let changed = profiles.iter().any(|p| {
            p.get("default_directory").and_then(toml::Value::as_str) != Some(&selected_workspace)
        });
        if changed {
            for p in profiles {
                p.as_table_mut()
                    .ok_or("invalid inventory profile tables")?
                    .insert(
                        "default_directory".into(),
                        toml::Value::String(selected_workspace.clone()),
                    );
            }
            config::load_inventory(&cfg.inventory_path)?;
            backup(&cfg.inventory_path)?;
            let output = toml::to_string(&value).map_err(|e| e.to_string())?;
            write_atomic(&cfg.inventory_path, output.as_bytes())?;
        }
    }
    if existing.is_none() {
        let text = format!(
            "schema_version = 1\nrole = \"home\"\ninventory_path = {}\nstate_dir = {}\n",
            toml::Value::String(cfg.inventory_path.to_string_lossy().into()),
            toml::Value::String(cfg.state_dir.to_string_lossy().into())
        );
        write_atomic(&home_path, text.as_bytes())?;
    }
    Ok(())
}
fn new_inventory_text(dir: &str) -> String {
    new_inventory(dir)
}
