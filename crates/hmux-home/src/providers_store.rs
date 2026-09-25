use super::*;
use hmux_core::PrivateDir;
use hmux_model::Profile;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::{SystemTime, UNIX_EPOCH},
};

fn read(home: &Path, relative: &str, limit: usize) -> Option<Vec<u8>> {
    read_existing(home, relative, limit).ok().flatten()
}
fn read_existing(
    home: &Path,
    relative: &str,
    limit: usize,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let path = home.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| ProviderError::new("invalid provider path"))?;
    match PrivateDir::open(parent) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(ProviderError::new(
                "provider credential directory is unsafe",
            ))
        }
    }
    let before = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ProviderError::new("provider credential file is unsafe")),
    };
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() > limit as u64
        || before.uid() != rustix::process::getuid().as_raw()
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
    {
        return Err(ProviderError::new("provider credential file is unsafe"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|_| ProviderError::new("provider credential file is unsafe"))?;
    let after = file
        .metadata()
        .map_err(|_| ProviderError::new("provider credential file is unsafe"))?;
    if before.dev() != after.dev() || before.ino() != after.ino() || after.len() > limit as u64 {
        return Err(ProviderError::new(
            "provider credential file changed while opening",
        ));
    }
    let mut bytes = Vec::with_capacity(after.len() as usize);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ProviderError::new("provider credential read failed"))?;
    if bytes.len() > limit {
        return Err(ProviderError::new("provider credential is too large"));
    }
    Ok(Some(bytes))
}
fn save(home: &Path, relative: &str, bytes: &[u8], max: usize) -> Result<(), ProviderError> {
    if bytes.len() > max {
        return Err(ProviderError::new("provider credential is too large"));
    }
    let path = home.join(relative);
    let parent = path
        .parent()
        .ok_or_else(|| ProviderError::new("invalid provider path"))?;
    let dir = PrivateDir::open_or_create_trusted(parent)
        .map_err(|_| ProviderError::new("provider credential directory is unsafe"))?;
    let name = path
        .file_name()
        .ok_or_else(|| ProviderError::new("invalid provider path"))?;
    let previous = read_existing(home, relative, max)?;
    if let Some(before) = previous {
        let backup = format!("{}.hmux-backup-{}", name.to_string_lossy(), stamp());
        dir.write_atomic_private(OsStr::new(&backup), &before)
            .map_err(|_| ProviderError::new("provider credential backup failed"))?;
    }
    dir.write_atomic_private(name, bytes)
        .map_err(|_| ProviderError::new("provider credential write failed"))
}
fn stamp() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ").to_string()
}
fn json_object(
    home: &Path,
    relative: &str,
    limit: usize,
) -> Result<(Map<String, Value>, Vec<u8>), ProviderError> {
    let before = read_existing(home, relative, limit)?.unwrap_or_default();
    if before.is_empty() {
        return Ok((Map::new(), before));
    }
    let Value::Object(map) = serde_json::from_slice::<Value>(&before)
        .map_err(|_| ProviderError::new("provider settings cannot be parsed"))?
    else {
        return Err(ProviderError::new("provider settings must be an object"));
    };
    Ok((map, before))
}
fn save_json(
    home: &Path,
    relative: &str,
    map: Map<String, Value>,
    limit: usize,
) -> Result<(), ProviderError> {
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(map))
        .map_err(|_| ProviderError::new("encode provider settings"))?;
    bytes.push(b'\n');
    save(home, relative, &bytes, limit)
}
fn valid_key(key: &str) -> bool {
    let n = key.len();
    (16..=512).contains(&n)
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
pub(super) fn hint(key: &str) -> String {
    if key.len() < 8 {
        return String::new();
    }
    let suffix: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{suffix}")
}
pub(super) fn claude_key(home: &Path) -> String {
    let Some(raw) = read(home, ".claude/settings.json", 1 << 20) else {
        return String::new();
    };
    serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("env")?
                .get("ANTHROPIC_API_KEY")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
pub(super) fn codex_hint(home: &Path) -> String {
    let Some(raw) = read(home, ".codex/auth.json", 1 << 20) else {
        return String::new();
    };
    serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|v| v.get("OPENAI_API_KEY")?.as_str().map(hint))
        .unwrap_or_default()
}
pub(super) fn dotenv_value(home: &Path, key: &str) -> String {
    let Some(raw) = read(home, ".gemini/.env", 1 << 20) else {
        return String::new();
    };
    let Ok(raw) = String::from_utf8(raw) else {
        return String::new();
    };
    raw.lines()
        .find_map(|line| {
            let trimmed = line.trim().strip_prefix("export ").unwrap_or(line.trim());
            let (name, val) = trimmed.split_once('=')?;
            (name.trim() == key).then(|| val.trim().trim_matches(['\"', '\'']).to_owned())
        })
        .unwrap_or_default()
}
pub(super) fn gemini_selected_auth(home: &Path) -> Result<String, ProviderError> {
    let (settings, _) = json_object(home, ".gemini/settings.json", 1 << 20)?;
    Ok(settings
        .get("security")
        .and_then(|v| v.get("auth"))
        .and_then(|v| v.get("selectedType"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned())
}
fn set_claude_key(home: &Path, name: &str, key: &str) -> Result<(), ProviderError> {
    let (mut settings, _) = json_object(home, ".claude/settings.json", 1 << 20)?;
    let mut env = match settings.remove("env") {
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(ProviderError::new(
                "~/.claude/settings.json의 env 항목이 객체가 아닙니다",
            ))
        }
        None => Map::new(),
    };
    if key.is_empty() {
        if env.remove(name).is_none() {
            return Ok(());
        }
    } else {
        env.insert(name.into(), Value::String(key.into()));
    }
    if !env.is_empty() {
        settings.insert("env".into(), Value::Object(env));
    }
    save_json(home, ".claude/settings.json", settings, 1 << 20)
}
fn set_dotenv(home: &Path, name: &str, value: &str) -> Result<(), ProviderError> {
    let raw = read_existing(home, ".gemini/.env", 1 << 20)?.unwrap_or_default();
    let content =
        String::from_utf8(raw).map_err(|_| ProviderError::new("Gemini .env cannot be parsed"))?;
    let mut found = false;
    let mut lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim().strip_prefix("export ").unwrap_or(line.trim());
        if trimmed
            .split_once('=')
            .is_some_and(|(k, _)| k.trim() == name)
        {
            if !found && !value.is_empty() {
                lines.push(format!("{name}={value}"));
            }
            found = true;
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found && !value.is_empty() {
        lines.push(format!("{name}={value}"));
    }
    if found || !value.is_empty() {
        let data = if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        };
        save(home, ".gemini/.env", data.as_bytes(), 1 << 20)?;
    }
    Ok(())
}
pub(super) fn set_gemini_auth(
    home: &Path,
    want: &str,
    replace: impl Fn(&str) -> bool,
) -> Result<(), ProviderError> {
    let (mut settings, _) = json_object(home, ".gemini/settings.json", 1 << 20)?;
    let mut security = match settings.remove("security") {
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(ProviderError::new(
                "~/.gemini/settings.json의 security 항목이 객체가 아닙니다",
            ))
        }
        None => Map::new(),
    };
    let mut auth = match security.remove("auth") {
        Some(Value::Object(map)) => map,
        Some(_) => {
            return Err(ProviderError::new(
                "~/.gemini/settings.json의 auth 항목이 객체가 아닙니다",
            ))
        }
        None => Map::new(),
    };
    let current = auth
        .get("selectedType")
        .and_then(Value::as_str)
        .unwrap_or("");
    if current == want || !replace(current) {
        return Ok(());
    }
    if want.is_empty() {
        auth.remove("selectedType");
    } else {
        auth.insert("selectedType".into(), Value::String(want.into()));
    }
    if auth.is_empty() {
        security.remove("auth");
    } else {
        security.insert("auth".into(), Value::Object(auth));
    }
    if security.is_empty() {
        settings.remove("security");
    } else {
        settings.insert("security".into(), Value::Object(security));
    }
    save_json(home, ".gemini/settings.json", settings, 1 << 20)
}
pub(super) fn oauth_fingerprint(home: &Path) -> Option<String> {
    let raw = read(home, ".gemini/oauth_creds.json", 1 << 20)?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let refresh = v.get("refresh_token").and_then(Value::as_str).unwrap_or("");
    if valid_token(refresh) {
        return Some(format!("refresh:{:x}", Sha256::digest(refresh.as_bytes())));
    }
    let access = v.get("access_token").and_then(Value::as_str).unwrap_or("");
    let expiry = v.get("expiry_date").and_then(Value::as_i64).unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    (valid_token(access) && expiry > now + 30_000).then(|| "access-only".into())
}
fn valid_token(s: &str) -> bool {
    (8..=16 * 1024).contains(&s.len()) && s.bytes().all(|b| b >= 0x21 && b != 0x7f)
}
pub(super) fn mark_claude_ready(
    home: &Path,
    key: &str,
    version: Option<&str>,
) -> Result<(), ProviderError> {
    let (mut state, _) = json_object(home, ".claude.json", 64 << 20)?;
    let mut changed = false;
    if state.get("hasCompletedOnboarding").and_then(Value::as_bool) != Some(true) {
        state.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        changed = true;
    }
    if !state
        .get("lastOnboardingVersion")
        .is_some_and(Value::is_string)
    {
        if let Some(version) = version {
            state.insert(
                "lastOnboardingVersion".into(),
                Value::String(version.into()),
            );
            changed = true;
        }
    }
    if !key.is_empty() {
        let suffix = &key[key.len().saturating_sub(20)..];
        let mut responses = match state.remove("customApiKeyResponses") {
            Some(Value::Object(map)) => map,
            Some(_) => {
                return Err(ProviderError::new(
                    "~/.claude.json의 customApiKeyResponses 항목이 객체가 아닙니다",
                ))
            }
            None => Map::new(),
        };
        let mut approved = match responses.remove("approved") {
            Some(Value::Array(a)) => a,
            Some(_) => {
                return Err(ProviderError::new(
                    "~/.claude.json의 approved 항목이 배열이 아닙니다",
                ))
            }
            None => Vec::new(),
        };
        if !approved.iter().any(|v| v.as_str() == Some(suffix)) {
            approved.push(Value::String(suffix.into()));
            changed = true;
        }
        responses.insert("approved".into(), Value::Array(approved));
        state.insert("customApiKeyResponses".into(), Value::Object(responses));
    }
    if changed {
        save_json(home, ".claude.json", state, 64 << 20)?;
    }
    Ok(())
}

impl ProviderService {
    pub(super) async fn private_task<T: Send + 'static>(
        &self,
        f: impl FnOnce(ProviderEnv) -> Result<T, ProviderError> + Send + 'static,
    ) -> Result<T, ProviderError> {
        let permit = self
            .native_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ProviderError::new("provider command busy"))?;
        let env = self.env.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f(env)
        })
        .await
        .map_err(|_| ProviderError::new("provider task failed"))?
    }
    pub(super) async fn set_key(
        &self,
        p: Provider,
        key: &str,
        cancel: &CancellationToken,
    ) -> Result<(), ProviderError> {
        if !key.is_empty() && !valid_key(key) {
            return Err(ProviderError::new("API 키 형식이 올바르지 않습니다"));
        }
        if cancel.is_cancelled() {
            return Err(ProviderError::new("provider request cancelled"));
        }
        match p.id {
            "codex" => {
                let path = self
                    .private_task(|env| Ok(env.executable("codex")))
                    .await?
                    .ok_or_else(|| ProviderError::new("Codex를 먼저 설치하세요"))?;
                if key.is_empty() {
                    let status = self.status(p, cancel).await;
                    if status.auth != "api-key" {
                        return Ok(());
                    }
                    self.command(&path, &["logout"], 64 * 1024, cancel)
                        .await
                        .map(|_| ())
                } else {
                    self.codex_login(&path, key, cancel).await
                }
            }
            "claude" => {
                let version = if !key.is_empty() {
                    if let Some(path) = self
                        .private_task(|env| Ok(env.executable("claude")))
                        .await?
                    {
                        self.command(&path, &["--version"], 64 * 1024, cancel)
                            .await
                            .ok()
                            .map(|raw| super::status::clean_version(&raw))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let key = key.to_owned();
                let name = p.key_name;
                self.private_task(move |env| {
                    set_claude_key(&env.home, name, &key)?;
                    if !key.is_empty() {
                        mark_claude_ready(&env.home, &key, version.as_deref())?;
                    }
                    Ok(())
                })
                .await
            }
            "gemini" => {
                let key = key.to_owned();
                let name = p.key_name;
                self.private_task(move |env| {
                    if key.is_empty() {
                        let selected = gemini_selected_auth(&env.home)?;
                        let next = if oauth_fingerprint(&env.home).is_some() {
                            "oauth-personal"
                        } else {
                            ""
                        };
                        set_dotenv(&env.home, name, "")?;
                        if selected == "gemini-api-key" {
                            set_gemini_auth(&env.home, next, |current| {
                                current == "gemini-api-key"
                            })?;
                        }
                        return Ok(());
                    }
                    set_dotenv(&env.home, name, &key)?;
                    set_gemini_auth(&env.home, "gemini-api-key", |_| true)?;
                    Ok(())
                })
                .await
            }
            _ => Err(ProviderError::new("unknown provider")),
        }
    }
    pub(super) async fn ensure_profile(&self, p: Provider) -> Result<(), ProviderError> {
        if self
            .private_task(move |env| Ok(env.executable(p.command)))
            .await?
            .is_none()
        {
            return Err(ProviderError::new(format!(
                "{} 설치를 확인하지 못했습니다",
                p.label
            )));
        }
        self.private_task(move |env| append_profile(&env.inventory_path, p))
            .await
    }
}
fn append_profile(path: &Path, p: Provider) -> Result<(), ProviderError> {
    let parent = path
        .parent()
        .ok_or_else(|| ProviderError::new("invalid inventory path"))?;
    let dir = PrivateDir::open(parent)
        .map_err(|_| ProviderError::new("Home inventory directory is unsafe"))?;
    let name = path
        .file_name()
        .ok_or_else(|| ProviderError::new("invalid inventory path"))?;
    let lock_name = format!("{}.lock", name.to_string_lossy());
    let _lock = dir
        .lock_for(OsStr::new(&lock_name), Duration::from_secs(2))
        .map_err(|_| ProviderError::new("Home inventory 잠금을 얻지 못했습니다"))?;
    let inventory = crate::config::load_inventory(path)
        .map_err(|_| ProviderError::new("Home inventory를 읽지 못했습니다"))?;
    let profiles = inventory.profiles.as_deref().unwrap_or_default();
    for profile in profiles {
        if profile
            .command
            .as_deref()
            .and_then(|x| x.first())
            .is_some_and(|s| Path::new(s).file_name() == Some(OsStr::new(p.command)))
        {
            return Ok(());
        }
        if profile.id == p.id {
            return Err(ProviderError::new(format!(
                "프로파일 ID {:?}가 이미 다른 명령에 쓰이고 있습니다",
                p.id
            )));
        }
    }
    let mut counts = std::collections::HashMap::new();
    let mut base = "";
    let mut best = 0;
    for profile in profiles {
        if profile.default_directory.is_empty() {
            continue;
        }
        let n = counts
            .entry(profile.default_directory.as_str())
            .or_insert(0usize);
        *n += 1;
        if *n > best {
            best = *n;
            base = &profile.default_directory;
        }
    }
    if base.is_empty() {
        base = "~/.hmux"
    }
    let profile = Profile {
        id: p.id.into(),
        label: p.label.into(),
        default_directory: base.into(),
        command: Some(vec![p.command.into()]),
        tags: Some(vec!["ai".into(), p.id.into()]),
    };
    let mut next = inventory.clone();
    next.profiles
        .get_or_insert_with(Vec::new)
        .push(profile.clone());
    next.validate()
        .map_err(|_| ProviderError::new("invalid provider profile"))?;
    let raw = read_inventory_raw(path)?;
    let mut table: toml::Value = toml::from_str(
        std::str::from_utf8(&raw).map_err(|_| ProviderError::new("Home inventory is not UTF-8"))?,
    )
    .map_err(|_| ProviderError::new("Home inventory cannot be parsed"))?;
    let arr = table
        .get_mut("profiles")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| ProviderError::new("invalid inventory profile tables"))?;
    let value =
        toml::Value::try_from(&profile).map_err(|_| ProviderError::new("encode profile"))?;
    arr.push(value);
    let output = toml::to_string(&table).map_err(|_| ProviderError::new("encode inventory"))?;
    let backup_name = format!("{}.hmux-backup-{}", name.to_string_lossy(), stamp());
    dir.write_atomic_private(OsStr::new(&backup_name), &raw)
        .map_err(|_| ProviderError::new("Home inventory backup failed"))?;
    dir.write_atomic_private(name, output.as_bytes())
        .map_err(|_| ProviderError::new("Home inventory write failed"))
}

fn read_inventory_raw(path: &Path) -> Result<Vec<u8>, ProviderError> {
    const LIMIT: u64 = 16 << 20;
    let before = std::fs::symlink_metadata(path)
        .map_err(|_| ProviderError::new("Home inventory is unsafe"))?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() == 0
        || before.len() > LIMIT
        || before.uid() != rustix::process::getuid().as_raw()
        || before.mode() & 0o022 != 0
        || before.nlink() != 1
    {
        return Err(ProviderError::new("Home inventory is unsafe"));
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ProviderError::new("Home inventory is unsafe"))?;
    let after = file
        .metadata()
        .map_err(|_| ProviderError::new("Home inventory is unsafe"))?;
    if before.dev() != after.dev() || before.ino() != after.ino() || after.len() > LIMIT {
        return Err(ProviderError::new("Home inventory changed while opening"));
    }
    let mut raw = Vec::with_capacity(after.len() as usize);
    file.take(LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|_| ProviderError::new("Home inventory read failed"))?;
    if raw.is_empty() || raw.len() as u64 > LIMIT {
        return Err(ProviderError::new("Home inventory size is invalid"));
    }
    Ok(raw)
}
