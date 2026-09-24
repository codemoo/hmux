//! Synchronous, bounded creation preparation and workspace allocation. No
//! tmux, provider, or metadata process is started here. An allocated directory
//! remains in place if a later step fails, matching the Go Home lifecycle.
use hmux_model::{Inventory, Profile};
use rustix::fs::{self as rfs, Mode, OFlags};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, DirBuilder, File},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
};

#[path = "create_plan_unicode.rs"]
mod go_unicode;

const MAX_NAME_SCALARS: usize = 80;
const MAX_FOLDER_SCALARS: usize = 36;
const MAX_PROFILE_SLUG_SCALARS: usize = 16;
const MAX_COMMAND_ARGS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_PATH_BYTES: usize = 8192;
const MAX_PATH_ENTRIES: usize = 128;
const ALLOCATION_ATTEMPTS: usize = 32;
const IDENTITY_FORMAT: &str = "#{session_id} #{session_created}";
const PROVIDER_SCRIPT: &str =
    "shell=$1; shift\ntrap ':' INT QUIT\n(trap - INT QUIT; exec \"$@\")\nexec \"$shell\" -i";

/// Fixed categories. No path, profile command, or requested name is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownProfile,
    Name,
    Directory,
    Command,
    Executable,
    Workspace,
    Entropy,
    Collision,
}

/// Validated work with no side effects. Debug never prints user configuration.
pub struct Plan {
    profile: Profile,
    folder: String,
    base: PathBuf,
    command: Vec<OsString>,
}
impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Plan([redacted])")
    }
}

/// A newly created 0700 child. Dropping it never deletes or reuses the folder.
pub struct Allocated {
    pub profile: Profile,
    pub directory: PathBuf,
    pub name: String,
    pub command: Vec<OsString>,
}
impl fmt::Debug for Allocated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Allocated([redacted])")
    }
}

impl Plan {
    /// The validated session name base used by the administrative dry run.
    pub fn folder(&self) -> &str {
        &self.folder
    }
    /// Validate without creating anything. `path` and `shell` are captured
    /// environment values, so retries cannot silently use changed settings.
    pub fn prepare(
        inventory: &Inventory,
        profile_id: &str,
        requested_name: &str,
        home: &Path,
        path: &OsStr,
        shell: &OsStr,
    ) -> Result<Self, Error> {
        let profile = inventory
            .profiles
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or(Error::UnknownProfile)?;
        hmux_model::validate_stable_id(&profile.id).map_err(|_| Error::Command)?;
        let display = if requested_name.is_empty() {
            profile.id.as_str()
        } else {
            requested_name
        };
        if display.chars().count() > MAX_NAME_SCALARS
            || display
                .chars()
                .any(|value| in_ranges(value, go_unicode::CONTROL))
        {
            return Err(Error::Name);
        }
        let folder = workspace_slug(display, MAX_FOLDER_SCALARS);
        let base = expand_home(&profile.default_directory, home)?;
        if base == Path::new("/") {
            return Err(Error::Directory);
        }
        match fs::metadata(&base) {
            Ok(metadata) if !metadata.is_dir() => return Err(Error::Directory),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::Directory)
            }
            _ => {}
        }
        let argv = profile.command.as_deref().ok_or(Error::Command)?;
        if argv.is_empty()
            || argv[0].is_empty()
            || argv.len() > MAX_COMMAND_ARGS
            || argv
                .iter()
                .any(|arg| arg.len() > MAX_ARGUMENT_BYTES || hmux_model::has_control(arg))
        {
            return Err(Error::Command);
        }
        let executable = executable_path(&argv[0], home, path)?;
        let mut command = Vec::with_capacity(argv.len() + 5);
        command.push(executable.into_os_string());
        command.extend(argv[1..].iter().map(OsString::from));
        if matches!(argv[0].as_str(), "codex" | "claude") {
            let mut wrapped = vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(PROVIDER_SCRIPT),
                OsString::from("hmux-provider"),
                provider_shell(shell),
            ];
            wrapped.extend(command);
            command = wrapped;
        } else if command.len() == 1 {
            // tmux parses a lone command through its default shell.
            command = vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from("exec \"$1\""),
                OsString::from("hmux-launch"),
                command.remove(0),
            ];
        }
        Ok(Self {
            profile: profile.clone(),
            folder,
            base,
            command,
        })
    }
}

impl Plan {
    /// Create one new child through the pinned administrator-selected base.
    /// Existing files, directories and symlinks cause a new candidate name.
    pub fn allocate(self) -> Result<Allocated, Error> {
        match fs::metadata(&self.base) {
            Ok(metadata) if !metadata.is_dir() => return Err(Error::Workspace),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&self.base)
                    .map_err(|_| Error::Workspace)?;
            }
            Err(_) => return Err(Error::Workspace),
        }
        // The administrator-selected base may be a symlink, as with Go OpenRoot.
        // mkdirat never follows a child link and never reuses an existing child.
        let base = File::from(
            rfs::open(
                &self.base,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| Error::Workspace)?,
        );
        for attempt in 0..ALLOCATION_ATTEMPTS {
            let mut random = [0u8; 6];
            getrandom::fill(&mut random).map_err(|_| Error::Entropy)?;
            let suffix = random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let child = if attempt == 0 {
                self.folder.clone()
            } else {
                format!("{}-{suffix}", self.folder)
            };
            match rfs::mkdirat(&base, child.as_str(), Mode::from_raw_mode(0o700)) {
                Ok(()) => {
                    let name = format!(
                        "{}-{}-{suffix}",
                        child,
                        workspace_slug(&self.profile.id, MAX_PROFILE_SLUG_SCALARS)
                    );
                    debug_assert!(name.chars().count() <= MAX_NAME_SCALARS);
                    return Ok(Allocated {
                        profile: self.profile,
                        directory: self.base.join(child),
                        name,
                        command: self.command,
                    });
                }
                Err(rustix::io::Errno::EXIST) => continue,
                Err(_) => return Err(Error::Workspace),
            }
        }
        Err(Error::Collision)
    }
}

impl Allocated {
    /// Exact argument array for tmux new-session. User data is never put in
    /// shell text; fixed wrappers consume positional arguments.
    pub fn tmux_args(&self) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("new-session"),
            OsString::from("-d"),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from(IDENTITY_FORMAT),
            OsString::from("-s"),
            OsString::from(&self.name),
            OsString::from("-c"),
            self.directory.as_os_str().to_owned(),
        ];
        args.extend(self.command.iter().cloned());
        args
    }
}

fn in_ranges(value: char, ranges: &[(u32, u32)]) -> bool {
    let value = u32::from(value);
    let index = ranges.partition_point(|(_, end)| *end < value);
    ranges
        .get(index)
        .is_some_and(|(start, end)| *start <= value && value <= *end)
}

fn workspace_slug(value: &str, limit: usize) -> String {
    let trimmed = value.trim_matches(|value| in_ranges(value, go_unicode::SPACE));
    let mut result = String::new();
    let mut count = 0;
    for value in trimmed.chars() {
        if in_ranges(value, go_unicode::LETTER_OR_NUMBER) || matches!(value, '_' | '-') {
            result.push(value);
            count += 1;
        } else if !result.is_empty() && !result.ends_with('-') {
            result.push('-');
            count += 1;
        }
        if count == limit {
            break;
        }
    }
    let slug = result.trim_matches(['-', '_']);
    if slug.is_empty() {
        "session".into()
    } else {
        slug.into()
    }
}

fn expand_home(raw: &str, home: &Path) -> Result<PathBuf, Error> {
    if raw.is_empty() || raw.len() > MAX_ARGUMENT_BYTES || hmux_model::has_control(raw) {
        return Err(Error::Directory);
    }
    let path = if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    };
    clean_absolute(&path)
}

fn clean_absolute(path: &Path) -> Result<PathBuf, Error> {
    if !path.is_absolute() || path.as_os_str().as_bytes().len() > MAX_PATH_BYTES {
        return Err(Error::Directory);
    }
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(part) => components.push(part.to_owned()),
            Component::ParentDir => {
                components.pop();
            }
            Component::Prefix(_) => return Err(Error::Directory),
        }
    }
    let mut clean = PathBuf::from("/");
    for component in components {
        clean.push(component);
    }
    Ok(clean)
}

fn usable_executable(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_bytes().len() <= MAX_PATH_BYTES
        && fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn executable_path(name: &str, home: &Path, path: &OsStr) -> Result<PathBuf, Error> {
    if name.is_empty() || name.contains('/') || name.len() > MAX_ARGUMENT_BYTES {
        return Err(Error::Command);
    }
    if path.as_bytes().len() > MAX_PATH_BYTES {
        return Err(Error::Executable);
    }
    for (index, raw_directory) in path.as_bytes().split(|byte| *byte == b':').enumerate() {
        if index == MAX_PATH_ENTRIES {
            return Err(Error::Executable);
        }
        let directory = Path::new(OsStr::from_bytes(raw_directory));
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(name);
        if usable_executable(&candidate) {
            return Ok(candidate);
        }
    }
    for directory in [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ] {
        let candidate = directory.join(name);
        if usable_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Executable)
}

fn provider_shell(inherited: &OsStr) -> OsString {
    for candidate in [
        PathBuf::from(inherited),
        PathBuf::from("/bin/zsh"),
        PathBuf::from("/bin/bash"),
        PathBuf::from("/bin/sh"),
    ] {
        if usable_executable(&candidate) {
            return candidate.into_os_string();
        }
    }
    OsString::from("/bin/sh")
}

#[cfg(test)]
#[path = "create_plan_tests.rs"]
mod tests;
