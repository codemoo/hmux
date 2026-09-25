//! Bounded, root-only Linux Gateway provisioning. No configuration is accepted from
//! environment variables and no credential material is passed in command arguments.
use crate::args::invalid;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::symlink,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_util::sync::CancellationToken;

const MARK: &str = "# Managed by hmux-web install-gateway; do not edit in place.\n";
const USAGE: &str = "usage: hmux-web install-gateway --domain DNS_NAME --https managed|external [--source-dir ABS] [--email EMAIL --accept-acme-terms] [--install-packages] [--connection-file ABS]";
const MAX_BUNDLE_FILES: usize = 2048;
const MAX_BUNDLE_BYTES: u64 = 384 * 1024 * 1024;
const ROOT: &str = "/opt/hmux-web";
const STATE: &str = "/var/lib/hmux-web";
const SERVICE: &str = "/etc/systemd/system/hmux-web.service";
const ENV: &str = "/etc/hmux-web/runtime.env";
const NGINX: &str = "/etc/nginx/conf.d/hmux.conf";
const HOOK: &str = "/etc/letsencrypt/renewal-hooks/deploy/hmux-web.sh";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Https {
    Managed,
    External,
}
#[derive(Debug)]
struct Options {
    source: PathBuf,
    domain: String,
    https: Https,
    email: Option<String>,
    packages: bool,
    connection: Option<PathBuf>,
}

fn clean_absolute(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        || path.components().collect::<PathBuf>() != path
    {
        return Err(invalid("path must be a clean absolute path"));
    }
    Ok(())
}
fn validate_domain(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 253
        || !value.is_ascii()
        || !value.contains('.')
        || value.ends_with('.')
        || value.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        || value.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err(invalid("--domain must be a DNS hostname"));
    }
    Ok(())
}
fn email(value: &str) -> io::Result<()> {
    if value.len() > 254
        || value.is_empty()
        || !value.is_ascii()
        || value.bytes().any(|b| {
            b <= 32 || b >= 127 || b == b'\\' || b == b'"' || b == b'\'' || b == b'<' || b == b'>'
        })
        || value.matches('@').count() != 1
    {
        return Err(invalid("--email must be a plain email address"));
    }
    let (local, host) = value.split_once('@').unwrap();
    if local.is_empty()
        || local.len() > 64
        || !local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&*+-/=?^_`{|}~".contains(&b))
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return Err(invalid("invalid email local part"));
    }
    validate_domain(host)
}
fn parse(args: &[OsString]) -> io::Result<Options> {
    if args.len() > 32 || args.iter().map(|a| a.len()).sum::<usize>() > 8192 {
        return Err(invalid("installation arguments exceed limit"));
    }
    let mut source = None;
    let mut host = None;
    let mut https = None;
    let mut mail = None;
    let mut terms = false;
    let mut packages = false;
    let mut connection = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index]
            .to_str()
            .ok_or_else(|| invalid("option must be UTF-8"))?;
        let (key, inline) = arg
            .split_once('=')
            .map_or((arg, None), |(k, v)| (k, Some(v)));
        if matches!(key, "--accept-acme-terms" | "--install-packages") {
            if inline.is_some() {
                return Err(invalid("switch does not accept a value"));
            }
            let slot = if key == "--accept-acme-terms" {
                &mut terms
            } else {
                &mut packages
            };
            if *slot {
                return Err(invalid("duplicate option"));
            }
            *slot = true;
        } else {
            let value = if let Some(v) = inline {
                v
            } else {
                index += 1;
                args.get(index)
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| invalid(USAGE))?
            };
            if value.is_empty() {
                return Err(invalid(USAGE));
            }
            match key {
                "--source-dir" if source.is_none() => source = Some(PathBuf::from(value)),
                "--domain" if host.is_none() => host = Some(value.to_owned()),
                "--https" if https.is_none() => {
                    https = Some(match value {
                        "managed" => Https::Managed,
                        "external" => Https::External,
                        _ => return Err(invalid("--https must be managed or external")),
                    })
                }
                "--email" if mail.is_none() => mail = Some(value.to_owned()),
                "--connection-file" if connection.is_none() => {
                    connection = Some(PathBuf::from(value))
                }
                _ => return Err(invalid(USAGE)),
            }
        }
        index += 1;
    }
    let source = source.unwrap_or(
        std::env::current_exe()?
            .parent()
            .ok_or_else(|| invalid("installer location unavailable"))?
            .to_path_buf(),
    );
    clean_absolute(&source)?;
    if let Some(path) = &connection {
        clean_absolute(path)?;
        if path.file_name().is_none() {
            return Err(invalid("invalid connection file"));
        }
    }
    let domain = host.ok_or_else(|| invalid("--domain is required"))?;
    validate_domain(&domain)?;
    let https = https.ok_or_else(|| invalid("--https is required"))?;
    match https {
        Https::Managed => {
            email(
                mail.as_deref()
                    .ok_or_else(|| invalid("--email is required for managed HTTPS"))?,
            )?;
            if !terms {
                return Err(invalid("--accept-acme-terms is required"));
            }
        }
        Https::External if mail.is_some() || terms || packages => {
            return Err(invalid("ACME/package options require managed HTTPS"))
        }
        _ => (),
    }
    Ok(Options {
        source,
        domain,
        https,
        email: mail,
        packages,
        connection,
    })
}

fn trusted_ancestors(path: &Path, allowed_uid: u32) -> io::Result<()> {
    clean_absolute(path)?;
    let mut current = PathBuf::from("/");
    for part in path.components().skip(1) {
        current.push(part);
        let m = fs::symlink_metadata(&current)?;
        if !m.is_dir()
            || !(m.uid() == 0 || m.uid() == allowed_uid)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(invalid("untrusted directory in path"));
        }
    }
    Ok(())
}
fn trusted_file(path: &Path, allowed_uid: u32, max: u64) -> io::Result<File> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_file()
        || !(m.uid() == 0 || m.uid() == allowed_uid)
        || m.mode() & 0o022 != 0
        || m.nlink() != 1
        || m.len() > max
    {
        return Err(invalid("untrusted bundle file"));
    }
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let opened = f.metadata()?;
    if opened.ino() != m.ino() || opened.dev() != m.dev() {
        return Err(invalid("bundle file changed during inspection"));
    }
    Ok(f)
}
fn bounded_digest(file: &mut File, expected_size: u64) -> io::Result<String> {
    if expected_size > MAX_BUNDLE_BYTES {
        return Err(invalid("bundle file exceeds limit"));
    }
    let mut remaining = expected_size;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 32768];
    while remaining > 0 {
        let wanted = remaining.min(buf.len() as u64) as usize;
        let n = file.read(&mut buf[..wanted])?;
        if n == 0 {
            return Err(invalid("bundle file changed during read"));
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    if file.read(&mut buf[..1])? != 0 {
        return Err(invalid("bundle file grew during read"));
    }
    Ok(format!("{:x}", hasher.finalize()))
}
#[cfg(test)]
fn hex_digest(path: &Path, uid: u32) -> io::Result<String> {
    let mut file = trusted_file(path, uid, MAX_BUNDLE_BYTES)?;
    let size = file.metadata()?.len();
    bounded_digest(&mut file, size)
}

struct BundleFile {
    rel: PathBuf,
    hash: String,
    size: u64,
}
struct VerifiedBundle {
    root: File,
    files: Vec<BundleFile>,
}
fn bundle_root(path: &Path, uid: u32) -> io::Result<File> {
    use rustix::fs::{openat, Mode, OFlags};
    clean_absolute(path)?;
    let mut dir = File::from(rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid("invalid bundle source"));
        };
        let next = File::from(openat(
            &dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let m = next.metadata()?;
        if !m.is_dir()
            || !(m.uid() == 0 || m.uid() == uid)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(invalid("untrusted directory in bundle path"));
        }
        dir = next;
    }
    Ok(dir)
}
fn bundle_child(dir: &File, name: &OsStr, uid: u32) -> io::Result<File> {
    use rustix::fs::{openat, Mode, OFlags};
    let file = File::from(openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let m = file.metadata()?;
    if !(m.uid() == 0 || m.uid() == uid)
        || m.mode() & 0o022 != 0
        || (m.is_file() && (m.nlink() != 1 || m.len() > MAX_BUNDLE_BYTES))
        || !(m.is_dir() || m.is_file())
    {
        return Err(invalid("untrusted bundle entry"));
    }
    Ok(file)
}
fn bundle_file(root: &File, rel: &Path, uid: u32) -> io::Result<File> {
    let mut dir = root.try_clone()?;
    let mut parts = rel.components().peekable();
    while let Some(component) = parts.next() {
        let Component::Normal(name) = component else {
            return Err(invalid("invalid bundle file path"));
        };
        let next = bundle_child(&dir, name, uid)?;
        if parts.peek().is_some() {
            if !next.metadata()?.is_dir() {
                return Err(invalid("bundle path component is not a directory"));
            }
            dir = next;
        } else if next.metadata()?.is_file() {
            return Ok(next);
        } else {
            return Err(invalid("bundle entry is not a file"));
        }
    }
    Err(invalid("empty bundle file path"))
}
fn verify_bundle(source: &Path, uid: u32) -> io::Result<VerifiedBundle> {
    let root = bundle_root(source, uid)?;
    let manifest_file = bundle_file(&root, Path::new("SHA256SUMS"), uid)?;
    if manifest_file.metadata()?.len() > 256 * 1024 {
        return Err(invalid("bundle manifest exceeds limit"));
    }
    let mut manifest_bytes = Vec::new();
    manifest_file
        .take(256 * 1024 + 1)
        .read_to_end(&mut manifest_bytes)?;
    if manifest_bytes.len() > 256 * 1024 {
        return Err(invalid("bundle manifest exceeds limit"));
    }
    let manifest =
        String::from_utf8(manifest_bytes).map_err(|_| invalid("bundle manifest must be UTF-8"))?;
    let mut listed = BTreeMap::new();
    for line in manifest.lines() {
        let (hash, name) = line
            .split_once("  ")
            .ok_or_else(|| invalid("invalid bundle manifest"))?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || name.len() > 512 {
            return Err(invalid("invalid bundle manifest"));
        }
        let rel = PathBuf::from(name.strip_prefix("./").unwrap_or(name));
        if rel.as_os_str().is_empty()
            || rel.is_absolute()
            || rel.components().any(|c| !matches!(c, Component::Normal(_)))
            || rel == Path::new("SHA256SUMS")
            || listed.insert(rel, hash.to_ascii_lowercase()).is_some()
        {
            return Err(invalid("invalid bundle manifest path"));
        }
    }
    let mut actual = Vec::new();
    let mut pending = vec![(root.try_clone()?, PathBuf::new())];
    let mut bytes = 0_u64;
    let mut entries = 0usize;
    while let Some((dir, prefix)) = pending.pop() {
        let mut listing = rustix::fs::Dir::read_from(&dir)?;
        while let Some(entry) = listing.read() {
            let entry = entry?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == OsStr::new(".") || name == OsStr::new("..") {
                continue;
            }
            entries += 1;
            if entries > MAX_BUNDLE_FILES * 2 {
                return Err(invalid("bundle entry count exceeds limit"));
            }
            let rel = prefix.join(name);
            let item = bundle_child(&dir, name, uid)?;
            let m = item.metadata()?;
            if m.is_dir() {
                pending.push((item, rel));
            } else if rel != Path::new("SHA256SUMS") {
                let Some(hash) = listed.get(&rel) else {
                    return Err(invalid("unlisted bundle file"));
                };
                bytes = bytes
                    .checked_add(m.len())
                    .ok_or_else(|| invalid("bundle too large"))?;
                actual.push(BundleFile {
                    rel,
                    hash: hash.clone(),
                    size: m.len(),
                });
            }
            if actual.len() > MAX_BUNDLE_FILES || bytes > MAX_BUNDLE_BYTES {
                return Err(invalid("bundle exceeds limits"));
            }
        }
    }
    if actual.len() != listed.len()
        || !listed.contains_key(Path::new("hmux-web"))
        || !listed.contains_key(Path::new("web/index.html"))
        || !listed.contains_key(Path::new("RELEASE"))
        || !listed.contains_key(Path::new("THIRD_PARTY_NOTICES.md"))
    {
        return Err(invalid("incomplete gateway bundle"));
    }
    for entry in &actual {
        let mut file = bundle_file(&root, &entry.rel, uid)?;
        if bounded_digest(&mut file, entry.size)? != entry.hash {
            return Err(invalid("bundle checksum mismatch"));
        }
    }
    let binary = bundle_file(&root, Path::new("hmux-web"), uid)?;
    if binary.metadata()?.mode() & 0o111 == 0 {
        return Err(invalid("gateway binary is not executable"));
    }
    let release_file = bundle_file(&root, Path::new("RELEASE"), uid)?;
    if release_file.metadata()?.len() > 4096 {
        return Err(invalid("release metadata exceeds limit"));
    }
    let mut release = String::new();
    release_file.take(4097).read_to_string(&mut release)?;
    if release.len() > 4096 || !release.lines().any(|s| s == "runtime=rust") {
        return Err(invalid("unsupported bundle runtime"));
    }
    actual.retain(|entry| {
        let p = entry.rel.as_path();
        p == Path::new("hmux-web")
            || p == Path::new("RELEASE")
            || p == Path::new("THIRD_PARTY_NOTICES.md")
            || p == Path::new("LICENSE")
            || p.starts_with("licenses")
            || p.starts_with("web")
    });
    Ok(VerifiedBundle {
        root,
        files: actual,
    })
}

fn active(stop: &CancellationToken) -> io::Result<()> {
    if stop.is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "gateway installation cancelled",
        ))
    } else {
        Ok(())
    }
}
fn executable(candidates: &[&str]) -> io::Result<PathBuf> {
    for path in candidates {
        let p = Path::new(path);
        if let Ok(m) = fs::symlink_metadata(p) {
            if m.is_file() && m.uid() == 0 && m.mode() & 0o022 == 0 && m.mode() & 0o111 != 0 {
                return Ok(p.to_path_buf());
            }
        }
    }
    Err(invalid("required trusted system tool unavailable"))
}
async fn command(
    candidates: &[&str],
    args: &[&str],
    stop: &CancellationToken,
    timeout: Duration,
) -> io::Result<String> {
    active(stop)?;
    let path = executable(candidates)?;
    let spec = hmux_core::command::CommandSpec::new(path.into_os_string(), 32 * 1024, timeout)
        .args(args.iter().map(OsString::from))
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C");
    let runner = hmux_core::command::CommandRunner::new(1).map_err(io::Error::other)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let running = runner.run_cancelable(spec, rx);
    tokio::pin!(running);
    let output = tokio::select! {
        result = &mut running => result,
        () = stop.cancelled() => { let _ = tx.send(()); running.await },
    }
    .map_err(io::Error::other)?;
    String::from_utf8(output.stdout).map_err(|_| invalid("system command returned invalid UTF-8"))
}
async fn system_command(
    candidates: &[&str],
    args: &[&str],
    stop: &CancellationToken,
    timeout: Duration,
) -> io::Result<()> {
    active(stop)?;
    let path = executable(candidates)?;
    let label = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut cmd = tokio::process::Command::new(path);
    cmd.env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = hmux_core::command::with_child_spawn(|| cmd.spawn())?;
    let group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .and_then(rustix::process::Pid::from_raw);
    let status = tokio::select! {
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(result) => result?,
            Err(_) => { kill_group(&mut child, group).await; return Err(io::Error::new(io::ErrorKind::TimedOut, "system command timed out")); }
        },
        () = stop.cancelled() => { kill_group(&mut child, group).await; return Err(io::Error::new(io::ErrorKind::Interrupted, "system command cancelled")); }
    };
    if !status.success() {
        return Err(io::Error::other(format!(
            "{label} exited unsuccessfully ({:?}); inspect its service/package diagnostics",
            status.code()
        )));
    }
    Ok(())
}
async fn kill_group(child: &mut tokio::process::Child, group: Option<rustix::process::Pid>) {
    if let Some(group) = group {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
    let _ = child.start_kill();
    let _ = child.wait().await;
}
async fn runuser_init_web(binary: &Path, stop: &CancellationToken) -> io::Result<()> {
    let program = binary
        .to_str()
        .ok_or_else(|| invalid("installed binary path must be UTF-8"))?;
    system_command(
        &["/usr/sbin/runuser", "/sbin/runuser"],
        &[
            "-u",
            "hmux-web",
            "--",
            program,
            "init-web",
            "--credentials",
            "/var/lib/hmux-web/credentials.json",
            "--token-file",
            "/var/lib/hmux-web/connector.token",
        ],
        stop,
        Duration::from_secs(60),
    )
    .await
}
fn checked_dir(path: &Path, mode: u32, uid: u32) -> io::Result<()> {
    clean_absolute(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("invalid target directory"))?;
    trusted_ancestors(parent, 0)?;
    match fs::create_dir(path) {
        Ok(()) => {
            fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir()
        || m.uid() != uid
        || m.mode() & 0o022 != 0
        || (mode == 0o700 && m.mode() & 0o077 != 0)
    {
        return Err(invalid("target directory is unmanaged or insecure"));
    }
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
fn stamp() -> io::Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    let mut random = [0_u8; 8];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    Ok(format!(
        "{}-{}-{:x}",
        now.as_secs(),
        now.subsec_nanos(),
        u64::from_le_bytes(random)
    ))
}
fn managed_original(path: &Path) -> io::Result<Option<(Vec<u8>, u32)>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(m) => {
            if !m.is_file()
                || m.uid() != managed_uid()
                || m.nlink() != 1
                || m.mode() & 0o022 != 0
                || m.len() > 1024 * 1024
            {
                return Err(invalid("conflicting unmanaged target file"));
            }
            let mut bytes = Vec::new();
            trusted_file(path, managed_uid(), 1024 * 1024)?.read_to_end(&mut bytes)?;
            if !is_marked(&bytes) {
                return Err(invalid("conflicting unmanaged target file"));
            }
            Ok(Some((bytes, m.mode() & 0o777)))
        }
    }
}
fn is_marked(bytes: &[u8]) -> bool {
    bytes.starts_with(MARK.as_bytes()) || bytes.starts_with(format!("#!/bin/sh\n{MARK}").as_bytes())
}
fn managed_uid() -> u32 {
    if cfg!(test) {
        rustix::process::geteuid().as_raw()
    } else {
        0
    }
}
type OriginalConfig = Option<(Vec<u8>, u32)>;
struct ConfigTxn {
    originals: Vec<(PathBuf, OriginalConfig)>,
    backups: PathBuf,
    id: String,
    committed: bool,
}
impl ConfigTxn {
    fn new(backups: PathBuf, id: String) -> Self {
        Self {
            originals: Vec::new(),
            backups,
            id,
            committed: false,
        }
    }
    fn write(&mut self, path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
        if !is_marked(bytes) {
            return Err(invalid("managed configuration lacks marker"));
        }
        if !self.originals.iter().any(|(p, _)| p == path) {
            let original = managed_original(path)?;
            if let Some((ref old, _)) = original {
                let name = format!("{}-{}.bak", self.id, self.originals.len());
                write_new(&self.backups.join(name), old, 0o600)?;
            }
            self.originals.push((path.to_path_buf(), original));
        }
        let temp = path.with_extension(format!("hmux-{}.tmp", self.id));
        write_new(&temp, bytes, mode)?;
        if let Err(e) = fs::rename(&temp, path) {
            let _ = fs::remove_file(temp);
            return Err(e);
        }
        Ok(())
    }
    fn rollback(&mut self) -> bool {
        let mut restored = true;
        for (path, original) in self.originals.iter().rev() {
            let result = match original {
                Some((bytes, mode)) => {
                    let temp = path.with_extension(format!("hmux-rollback-{}.tmp", self.id));
                    write_new(&temp, bytes, *mode).and_then(|()| fs::rename(&temp, path))
                }
                None => match fs::remove_file(path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    result => result,
                },
            };
            if result.is_err() {
                restored = false;
            }
        }
        self.committed = true;
        restored
    }
    fn commit(&mut self) {
        self.committed = true;
    }
}
impl Drop for ConfigTxn {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.rollback();
        }
    }
}

fn service_text() -> String {
    format!("{MARK}[Unit]\nDescription=HMux authenticated web gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser=hmux-web\nGroup=hmux-web\nWorkingDirectory=/opt/hmux-web/current\nEnvironmentFile=/etc/hmux-web/runtime.env\nExecStart=/opt/hmux-web/current/hmux-web serve --listen 127.0.0.1:8088 --origin ${{HMUX_WEB_ORIGIN}} --assets /opt/hmux-web/current/web --credentials /var/lib/hmux-web/credentials.json --token-file /var/lib/hmux-web/connector.token\nRestart=on-failure\nRestartSec=3\nUMask=0077\nStateDirectory=hmux-web\nStateDirectoryMode=0700\nNoNewPrivileges=true\nPrivateTmp=true\nPrivateDevices=true\nProtectSystem=strict\nProtectHome=true\nProtectKernelTunables=true\nProtectKernelModules=true\nProtectControlGroups=true\nRestrictSUIDSGID=true\nRestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX\nLockPersonality=true\nCapabilityBoundingSet=\nAmbientCapabilities=\nMemoryMax=256M\nTasksMax=128\nLimitNOFILE=512\nTimeoutStopSec=10\n\n[Install]\nWantedBy=multi-user.target\n")
}
fn challenge_site(host: &str) -> String {
    format!("{MARK}server {{\n listen 80;\n listen [::]:80;\n server_name {host};\n access_log off;\n location /.well-known/acme-challenge/ {{ root /var/www/hmux-acme; }}\n location / {{ return 503; }}\n}}\n")
}
fn https_site(host: &str) -> String {
    format!("{MARK}limit_req_zone $binary_remote_addr zone=hmux_auth:1m rate=5r/m;\nlimit_conn_zone $binary_remote_addr zone=hmux_clients:1m;\nserver {{\n listen 80;\n listen [::]:80;\n server_name {host};\n access_log off;\n location /.well-known/acme-challenge/ {{ root /var/www/hmux-acme; }}\n location / {{ return 301 https://$host$request_uri; }}\n}}\nserver {{\n listen 443 ssl;\n listen [::]:443 ssl;\n server_name {host};\n ssl_certificate /etc/letsencrypt/live/{host}/fullchain.pem;\n ssl_certificate_key /etc/letsencrypt/live/{host}/privkey.pem;\n ssl_protocols TLSv1.2 TLSv1.3;\n ssl_session_tickets off;\n add_header Strict-Transport-Security \"max-age=31536000\" always;\n client_max_body_size 16k;\n limit_conn hmux_clients 24;\n access_log off;\n location = /api/login {{\n  limit_req zone=hmux_auth burst=3 nodelay;\n  limit_req_status 429;\n  proxy_pass http://127.0.0.1:8088;\n  proxy_set_header Host $host;\n  proxy_set_header X-Real-IP $remote_addr;\n }}\n location / {{\n  proxy_pass http://127.0.0.1:8088;\n  proxy_http_version 1.1;\n  proxy_set_header Host $host;\n  proxy_set_header X-Real-IP $remote_addr;\n  proxy_set_header Upgrade $http_upgrade;\n  proxy_set_header Connection \"upgrade\";\n  proxy_set_header X-Forwarded-Proto https;\n  proxy_buffering off;\n  proxy_read_timeout 60s;\n  proxy_send_timeout 60s;\n }}\n}}\n")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SecretState {
    Empty,
    Pending,
    Ready,
}
fn secret_state(uid: u32) -> io::Result<SecretState> {
    secret_state_at(Path::new(STATE), uid)
}
fn secret_state_at(state: &Path, uid: u32) -> io::Result<SecretState> {
    let mut present = [false; 3];
    let mut connector_value = None;
    for (i, name) in [
        "credentials.json",
        "connector.token",
        "credentials.json.bootstrap",
    ]
    .iter()
    .enumerate()
    {
        let path = state.join(name);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
            Ok(m) => {
                if !m.is_file()
                    || m.uid() != uid
                    || m.nlink() != 1
                    || m.mode() & 0o077 != 0
                    || m.len() == 0
                    || m.len() > (if i == 0 { 4096 } else { 128 })
                {
                    return Err(invalid(
                        "existing gateway secret has unsafe ownership or mode",
                    ));
                }
                present[i] = true;
            }
        }
    }
    if present[0] != present[1] && !(present[1] && present[2] && !present[0]) {
        return Err(invalid("partial gateway enrollment: preserve existing files and recover the missing credential or token before reinstalling"));
    }
    if present[1] {
        let mut token = String::new();
        trusted_file(&state.join("connector.token"), uid, 128)?.read_to_string(&mut token)?;
        let token = token.trim_end_matches('\n');
        validate_token(token)?;
        connector_value = Some(token.to_owned());
    }
    if present[2] {
        let mut bootstrap = String::new();
        trusted_file(&state.join("credentials.json.bootstrap"), uid, 128)?
            .read_to_string(&mut bootstrap)?;
        let bootstrap = bootstrap.trim_end_matches('\n');
        validate_token(bootstrap)?;
        if !present[0] && connector_value.as_deref() == Some(bootstrap) {
            return Err(invalid("setup and connector tokens must differ"));
        }
    }
    if present[0] {
        let mut creds = Vec::new();
        trusted_file(&state.join("credentials.json"), uid, 4096)?.read_to_end(&mut creds)?;
        hmux_gateway::auth::parse_credentials(&creds)
            .map_err(|_| invalid("existing gateway credentials are invalid"))?;
    }
    match present {
        [false, false, false] => Ok(SecretState::Empty),
        [false, true, true] => Ok(SecretState::Pending),
        [true, true, _] => Ok(SecretState::Ready),
        _ => Err(invalid(
            "partial gateway enrollment: recover files before reinstalling",
        )),
    }
}
fn validate_token(value: &str) -> io::Result<()> {
    if value.len() != 43
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid("connector token has invalid format"));
    }
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid("connector token has invalid encoding"))?;
    if decoded.len() != 32 {
        return Err(invalid("connector token has invalid length"));
    }
    Ok(())
}
fn stage_release(bundle: &VerifiedBundle, id: &str, uid: u32) -> io::Result<PathBuf> {
    let releases = Path::new(ROOT).join("releases");
    checked_dir(Path::new(ROOT), 0o755, 0)?;
    checked_dir(&releases, 0o755, 0)?;
    stage_release_at(&releases, bundle, id, uid)
}
fn stage_release_at(
    releases: &Path,
    bundle: &VerifiedBundle,
    id: &str,
    uid: u32,
) -> io::Result<PathBuf> {
    let release = releases.join(id);
    fs::create_dir(&release)?;
    fs::set_permissions(&release, fs::Permissions::from_mode(0o700))?;
    let staged = (|| -> io::Result<()> {
        let mut sums = String::new();
        let mut directories = BTreeSet::new();
        for entry in &bundle.files {
            let target = release.join(&entry.rel);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                let mut ancestor = release.clone();
                for component in entry.rel.parent().into_iter().flat_map(Path::components) {
                    if let Component::Normal(name) = component {
                        ancestor.push(name);
                        fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o700))?;
                        directories.insert(ancestor.clone());
                    }
                }
            }
            let mut input = bundle_file(&bundle.root, &entry.rel, uid)?;
            if input.metadata()?.len() != entry.size {
                return Err(invalid("bundle file changed during staging"));
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&target)?;
            let mut remaining = entry.size;
            let mut hasher = Sha256::new();
            let mut buf = [0_u8; 32768];
            while remaining > 0 {
                let wanted = remaining.min(buf.len() as u64) as usize;
                let n = input.read(&mut buf[..wanted])?;
                if n == 0 {
                    return Err(invalid("bundle file changed during staging"));
                }
                output.write_all(&buf[..n])?;
                hasher.update(&buf[..n]);
                remaining -= n as u64;
            }
            if input.read(&mut buf[..1])? != 0 {
                return Err(invalid("bundle file grew during staging"));
            }
            let digest = format!("{:x}", hasher.finalize());
            if digest != entry.hash {
                return Err(invalid("bundle changed during installation"));
            }
            output.sync_all()?;
            sums.push_str(&format!("{digest}  ./{}\n", entry.rel.display()));
        }
        write_new(&release.join("SHA256SUMS"), sums.as_bytes(), 0o600)?;
        // The release root remains 0700 until every byte and mode is verified.
        for entry in &bundle.files {
            let mode = if entry.rel == Path::new("hmux-web") {
                0o755
            } else {
                0o644
            };
            fs::set_permissions(release.join(&entry.rel), fs::Permissions::from_mode(mode))?;
        }
        fs::set_permissions(
            release.join("SHA256SUMS"),
            fs::Permissions::from_mode(0o644),
        )?;
        for dir in directories.iter().rev() {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755))?;
        }
        fs::set_permissions(&release, fs::Permissions::from_mode(0o755))?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&release);
        return Err(error);
    }
    Ok(release)
}

fn old_pointer() -> io::Result<Option<PathBuf>> {
    let current = Path::new(ROOT).join("current");
    match fs::symlink_metadata(&current) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
        Ok(m) => {
            if !m.file_type().is_symlink() || m.uid() != 0 {
                return Err(invalid("unmanaged current release pointer"));
            }
            let target = fs::read_link(current)?;
            if !target.starts_with(Path::new(ROOT).join("releases")) || !target.is_dir() {
                return Err(invalid("unmanaged current release pointer"));
            }
            Ok(Some(target))
        }
    }
}
fn switch_pointer(target: &Path, id: &str) -> io::Result<()> {
    let temp = Path::new(ROOT).join(format!(".current-{id}"));
    symlink(target, &temp)?;
    if let Err(e) = fs::rename(&temp, Path::new(ROOT).join("current")) {
        let _ = fs::remove_file(temp);
        return Err(e);
    }
    Ok(())
}
fn restore_pointer(previous: Option<&Path>, id: &str) -> io::Result<()> {
    if let Some(previous) = previous {
        switch_pointer(previous, &format!("rollback-{id}"))
    } else {
        match fs::remove_file(Path::new(ROOT).join("current")) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}
async fn service_user(stop: &CancellationToken) -> io::Result<u32> {
    let uid = match command(
        &["/usr/bin/id", "/bin/id"],
        &["-u", "hmux-web"],
        stop,
        Duration::from_secs(10),
    )
    .await
    {
        Ok(text) => text
            .trim()
            .parse::<u32>()
            .map_err(|_| invalid("invalid hmux-web account"))?,
        Err(_) => {
            active(stop)?;
            system_command(
                &["/usr/sbin/useradd", "/sbin/useradd"],
                &[
                    "--system",
                    "--user-group",
                    "--home-dir",
                    STATE,
                    "--shell",
                    "/usr/sbin/nologin",
                    "hmux-web",
                ],
                stop,
                Duration::from_secs(30),
            )
            .await?;
            command(
                &["/usr/bin/id", "/bin/id"],
                &["-u", "hmux-web"],
                stop,
                Duration::from_secs(10),
            )
            .await?
            .trim()
            .parse::<u32>()
            .map_err(|_| invalid("invalid hmux-web account"))?
        }
    };
    if uid == 0 {
        return Err(invalid("gateway service user must not be root"));
    }
    let group = command(
        &["/usr/bin/id", "/bin/id"],
        &["-gn", "hmux-web"],
        stop,
        Duration::from_secs(10),
    )
    .await?;
    if group.trim() != "hmux-web" {
        return Err(invalid("gateway service user must have dedicated group"));
    }
    let account = command(
        &["/usr/bin/getent", "/bin/getent"],
        &["passwd", "hmux-web"],
        stop,
        Duration::from_secs(10),
    )
    .await?;
    let fields: Vec<_> = account.trim_end().split(':').collect();
    if fields.len() < 7
        || fields[0] != "hmux-web"
        || fields[2] != uid.to_string()
        || fields[5] != STATE
        || !matches!(fields[6], "/usr/sbin/nologin" | "/sbin/nologin")
    {
        return Err(invalid(
            "existing hmux-web account has unexpected home or login shell",
        ));
    }
    Ok(uid)
}
async fn prepare_https(opts: &Options, stop: &CancellationToken) -> io::Result<()> {
    if opts.https == Https::External {
        return Ok(());
    }
    let nginx = executable(&["/usr/sbin/nginx", "/sbin/nginx"]);
    let certbot = executable(&["/usr/bin/certbot", "/usr/sbin/certbot"]);
    if nginx.is_err() || certbot.is_err() {
        if !opts.packages || !Path::new("/etc/debian_version").is_file() {
            return Err(invalid(
                "nginx and certbot required; on apt-based Linux rerun with --install-packages",
            ));
        }
        system_command(
            &["/usr/bin/apt-get"],
            &["update"],
            stop,
            Duration::from_secs(1200),
        )
        .await?;
        system_command(
            &["/usr/bin/apt-get"],
            &["install", "-y", "nginx", "certbot"],
            stop,
            Duration::from_secs(1200),
        )
        .await?;
        executable(&["/usr/sbin/nginx", "/sbin/nginx"])?;
        executable(&["/usr/bin/certbot", "/usr/sbin/certbot"])?;
    }
    Ok(())
}
fn private_key_policy(key: &Path, domain: &str) -> io::Result<()> {
    let target = fs::canonicalize(key)?;
    if !target.starts_with(Path::new("/etc/letsencrypt/archive").join(domain)) {
        return Err(invalid("certificate key is outside the managed archive"));
    }
    trusted_ancestors(
        target
            .parent()
            .ok_or_else(|| invalid("invalid certificate key"))?,
        0,
    )?;
    let meta = fs::symlink_metadata(&target)?;
    if !meta.is_file()
        || meta.uid() != 0
        || meta.nlink() != 1
        || meta.mode() & 0o077 != 0
        || meta.len() == 0
        || meta.len() > 1024 * 1024
    {
        return Err(invalid("certificate key is not private"));
    }
    Ok(())
}

async fn managed_https(
    opts: &Options,
    txn: &mut ConfigTxn,
    stop: &CancellationToken,
) -> io::Result<()> {
    checked_dir(Path::new("/var/www"), 0o755, 0)?;
    checked_dir(Path::new("/var/www/hmux-acme"), 0o755, 0)?;
    checked_dir(Path::new("/etc/nginx/conf.d"), 0o755, 0)?;
    // A reinstall with its existing certificate and managed HTTPS site keeps
    // port 443 serving throughout renewal. Only fresh sites use ACME-only HTTP.
    let existing = managed_original(Path::new(NGINX))?;
    let certified = Path::new("/etc/letsencrypt/live")
        .join(&opts.domain)
        .join("fullchain.pem")
        .is_file()
        && Path::new("/etc/letsencrypt/live")
            .join(&opts.domain)
            .join("privkey.pem")
            .is_file();
    let same_site = existing.as_ref().is_some_and(|(bytes, _)| {
        let text = String::from_utf8_lossy(bytes);
        text.contains(&format!("server_name {};", opts.domain)) && text.contains("listen 443 ssl")
    });
    if !(certified && same_site) {
        txn.write(
            Path::new(NGINX),
            challenge_site(&opts.domain).as_bytes(),
            0o644,
        )?;
        system_command(
            &["/usr/sbin/nginx", "/sbin/nginx"],
            &["-t"],
            stop,
            Duration::from_secs(20),
        )
        .await?;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["enable", "--now", "nginx.service"],
            stop,
            Duration::from_secs(45),
        )
        .await?;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["reload", "nginx.service"],
            stop,
            Duration::from_secs(30),
        )
        .await?;
    }
    system_command(
        &["/usr/bin/certbot", "/usr/sbin/certbot"],
        &[
            "certonly",
            "--webroot",
            "-w",
            "/var/www/hmux-acme",
            "--non-interactive",
            "--agree-tos",
            "--email",
            opts.email.as_deref().unwrap_or_default(),
            "--domain",
            &opts.domain,
            "--cert-name",
            &opts.domain,
            "--keep-until-expiring",
        ],
        stop,
        Duration::from_secs(600),
    )
    .await?;
    let cert = Path::new("/etc/letsencrypt/live")
        .join(&opts.domain)
        .join("fullchain.pem");
    let key = Path::new("/etc/letsencrypt/live")
        .join(&opts.domain)
        .join("privkey.pem");
    if !cert.is_file() || !key.is_file() {
        return Err(invalid(
            "certbot reported success without certificate files",
        ));
    }
    private_key_policy(&key, &opts.domain)?;
    txn.write(Path::new(NGINX), https_site(&opts.domain).as_bytes(), 0o644)?;
    system_command(
        &["/usr/sbin/nginx", "/sbin/nginx"],
        &["-t"],
        stop,
        Duration::from_secs(20),
    )
    .await?;
    system_command(
        &["/usr/bin/systemctl", "/bin/systemctl"],
        &["reload", "nginx.service"],
        stop,
        Duration::from_secs(30),
    )
    .await?;
    for path in [
        "/etc/letsencrypt",
        "/etc/letsencrypt/renewal-hooks",
        "/etc/letsencrypt/renewal-hooks/deploy",
    ] {
        checked_dir(Path::new(path), 0o755, 0)?;
    }
    let hook = format!(
        "#!/bin/sh\n{MARK}set -eu\n/usr/sbin/nginx -t\n/usr/bin/systemctl reload nginx.service\n"
    );
    txn.write(Path::new(HOOK), hook.as_bytes(), 0o755)?;
    system_command(
        &["/usr/bin/systemctl", "/bin/systemctl"],
        &["enable", "--now", "certbot.timer"],
        stop,
        Duration::from_secs(45),
    )
    .await?;
    Ok(())
}

fn export_owner() -> io::Result<(u32, u32)> {
    let uid = match std::env::var("SUDO_UID") {
        Ok(s) => s.parse::<u32>().map_err(|_| invalid("invalid SUDO_UID"))?,
        Err(_) => 0,
    };
    let gid = match std::env::var("SUDO_GID") {
        Ok(s) => s.parse::<u32>().map_err(|_| invalid("invalid SUDO_GID"))?,
        Err(_) if uid == 0 => 0,
        Err(_) => return Err(invalid("SUDO_GID required for connection export")),
    };
    if uid == 0 && gid != 0 {
        return Err(invalid("invalid invoking account"));
    }
    Ok((uid, gid))
}
fn export_parent(path: &Path, uid: u32) -> io::Result<File> {
    use rustix::fs::{openat, Mode, OFlags};
    clean_absolute(path)?;
    let mut current = File::from(rustix::fs::open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(invalid("invalid export directory"));
        };
        let next = File::from(openat(
            &current,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let m = next.metadata()?;
        if !m.is_dir()
            || !(m.uid() == 0 || m.uid() == uid)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(invalid("untrusted connection export ancestor"));
        }
        current = next;
    }
    let m = current.metadata()?;
    if m.uid() != uid || m.mode() & 0o077 != 0 {
        return Err(invalid(
            "connection export directory must be private and owned by invoking user",
        ));
    }
    Ok(current)
}
async fn export_connection(
    path: &Path,
    domain: &str,
    service_uid: u32,
    stop: &CancellationToken,
) -> io::Result<()> {
    let (uid, gid) = export_owner()?;
    if uid != 0 {
        let id = uid.to_string();
        let entry = command(
            &["/usr/bin/getent", "/bin/getent"],
            &["passwd", &id],
            stop,
            Duration::from_secs(10),
        )
        .await?;
        let fields: Vec<_> = entry.trim_end().split(':').collect();
        if fields.len() < 4 || fields[2] != id || fields[3] != gid.to_string() {
            return Err(invalid("SUDO_UID/GID do not match an OS account"));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("invalid connection file"))?;
    let directory = export_parent(parent, uid)?;
    let mut token = String::new();
    trusted_file(&Path::new(STATE).join("connector.token"), service_uid, 128)?
        .read_to_string(&mut token)?;
    let token = token.trim_end_matches('\n');
    validate_token(token)?;
    let json =
        serde_json::json!({"schema":1,"endpoint":format!("wss://{domain}/connect"),"token":token});
    let data = serde_json::to_vec(&json).map_err(io::Error::other)?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("invalid connection file"))?;
    let mut file = File::from(rustix::fs::openat(
        &directory,
        name,
        rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::from_raw_mode(0o600),
    )?);
    file.write_all(&data)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    if uid != 0 {
        rustix::fs::fchown(
            &file,
            Some(rustix::process::Uid::from_raw(uid)),
            Some(rustix::process::Gid::from_raw(gid)),
        )
        .map_err(io::Error::from)?;
    }
    Ok(())
}

async fn gateway_probe(address: std::net::SocketAddr, domain: &str) -> io::Result<bool> {
    let mut socket = TcpStream::connect(address).await?;
    socket
        .write_all(
            format!(
                "GET /api/setup/status HTTP/1.1\r\nHost: {domain}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    let mut raw = Vec::new();
    socket.take(4097).read_to_end(&mut raw).await?;
    if raw.len() > 4096 {
        return Ok(false);
    }
    let response =
        std::str::from_utf8(&raw).map_err(|_| invalid("invalid gateway health response"))?;
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        return Ok(false);
    };
    Ok(
        (head.starts_with("HTTP/1.1 200 ") || head.starts_with("HTTP/1.0 200 "))
            && (body.contains("\"required\":true") || body.contains("\"required\":false")),
    )
}
async fn await_gateway_ready_at(
    address: std::net::SocketAddr,
    domain: &str,
    stop: &CancellationToken,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        active(stop)?;
        let probe = tokio::time::timeout(Duration::from_secs(2), gateway_probe(address, domain));
        tokio::select! {
            biased;
            () = stop.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "gateway installation cancelled")),
            result = probe => if matches!(result, Ok(Ok(true))) { return Ok(()); },
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "gateway did not become ready",
            ));
        }
        tokio::select! {
            biased;
            () = stop.cancelled() => return Err(io::Error::new(io::ErrorKind::Interrupted, "gateway installation cancelled")),
            () = tokio::time::sleep(Duration::from_millis(250)) => {},
        }
    }
}

pub async fn run(args: &[OsString], stop: CancellationToken) -> io::Result<()> {
    if args == ["--help"] || args == ["-h"] {
        println!("{USAGE}");
        return Ok(());
    }
    if !cfg!(target_os = "linux") {
        return Err(invalid("Gateway installation requires Linux"));
    }
    let options = parse(args)?;
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(invalid(
            "Gateway installation requires root; use the installer sudo wrapper",
        ));
    }
    active(&stop)?;
    if !Path::new("/run/systemd/system").is_dir() {
        return Err(invalid(
            "a running systemd system manager is required for Gateway installation",
        ));
    }
    executable(&["/usr/bin/systemctl", "/bin/systemctl"])?;
    let (invoking_uid, _) = export_owner()?;
    let files = verify_bundle(&options.source, invoking_uid)?;
    if let Some(path) = &options.connection {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("invalid connection export path"))?;
        let _ = export_parent(parent, invoking_uid)?;
        if fs::symlink_metadata(path).is_ok() {
            return Err(invalid("connection export destination already exists"));
        }
    }
    // Hold the singleton lock before snapshotting mutable managed state.
    checked_dir(Path::new("/etc/hmux-web"), 0o755, 0)?;
    let lock_dir = hmux_core::PrivateDir::open_existing_trusted(Path::new("/etc/hmux-web"))?;
    let _lock = lock_dir
        .lock_for(OsStr::new("install.lock"), Duration::from_secs(10))
        .map_err(io::Error::other)?;
    // Existing managed files and pointer are checked before any package/account mutation.
    for path in [SERVICE, ENV] {
        managed_original(Path::new(path))?;
    }
    if options.https == Https::Managed {
        managed_original(Path::new(NGINX))?;
        managed_original(Path::new(HOOK))?;
    }
    let previous = old_pointer()?;
    let id = stamp()?;
    checked_dir(Path::new("/etc/hmux-web/backups"), 0o700, 0)?;
    prepare_https(&options, &stop).await?;
    let service_uid = service_user(&stop).await?;
    if !Path::new(STATE).exists() {
        checked_dir(Path::new("/var/lib"), 0o755, 0)?;
        system_command(
            &["/usr/bin/install", "/bin/install"],
            &[
                "-d", "-m", "0700", "-o", "hmux-web", "-g", "hmux-web", STATE,
            ],
            &stop,
            Duration::from_secs(15),
        )
        .await?;
    }
    checked_dir(Path::new(STATE), 0o700, service_uid)?;
    let mut enrollment = secret_state(service_uid)?;
    let release = stage_release(&files, &id, invoking_uid)?;
    if enrollment == SecretState::Empty {
        runuser_init_web(&release.join("hmux-web"), &stop).await?;
        enrollment = secret_state(service_uid)?;
        if enrollment != SecretState::Pending {
            return Err(invalid(
                "web bootstrap initializer did not create the expected private files",
            ));
        }
    }
    let mut txn = ConfigTxn::new(Path::new("/etc/hmux-web/backups").to_path_buf(), id.clone());
    let mut activation_attempted = false;
    let result: io::Result<()> = async {
        active(&stop)?;
        txn.write(
            Path::new(ENV),
            format!("{MARK}HMUX_WEB_ORIGIN=https://{}\n", options.domain).as_bytes(),
            0o644,
        )?;
        txn.write(Path::new(SERVICE), service_text().as_bytes(), 0o644)?;
        if options.https == Https::Managed {
            managed_https(&options, &mut txn, &stop).await?;
        }
        switch_pointer(&release, &id)?;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["daemon-reload"],
            &stop,
            Duration::from_secs(30),
        )
        .await?;
        activation_attempted = true;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["enable", "hmux-web.service"],
            &stop,
            Duration::from_secs(45),
        )
        .await?;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["restart", "hmux-web.service"],
            &stop,
            Duration::from_secs(45),
        )
        .await?;
        await_gateway_ready_at(
            "127.0.0.1:8088".parse().expect("literal loopback"),
            &options.domain,
            &stop,
        )
        .await?;
        system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["is-active", "--quiet", "hmux-web.service"],
            &stop,
            Duration::from_secs(10),
        )
        .await?;
        if let Some(path) = &options.connection {
            export_connection(path, &options.domain, service_uid, &stop).await?;
        }
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let recovery = CancellationToken::new();
        let mut restored = true;
        // A fresh unit must be stopped while systemd can still read its file.
        if previous.is_none() && activation_attempted {
            restored &= system_command(
                &["/usr/bin/systemctl", "/bin/systemctl"],
                &["disable", "--now", "hmux-web.service"],
                &recovery,
                Duration::from_secs(30),
            )
            .await
            .is_ok();
        }
        if !txn.rollback() {
            restored = false;
        }
        if restore_pointer(previous.as_deref(), &id).is_err() {
            restored = false;
        }
        if system_command(
            &["/usr/bin/systemctl", "/bin/systemctl"],
            &["daemon-reload"],
            &recovery,
            Duration::from_secs(15),
        )
        .await
        .is_err()
        {
            restored = false;
        }
        if previous.is_some() {
            restored &= system_command(
                &["/usr/bin/systemctl", "/bin/systemctl"],
                &["restart", "hmux-web.service"],
                &recovery,
                Duration::from_secs(30),
            )
            .await
            .is_ok();
        }
        if options.https == Https::Managed {
            if system_command(
                &["/usr/sbin/nginx", "/sbin/nginx"],
                &["-t"],
                &recovery,
                Duration::from_secs(15),
            )
            .await
            .is_err()
            {
                restored = false;
            }
            if system_command(
                &["/usr/bin/systemctl", "/bin/systemctl"],
                &["reload", "nginx.service"],
                &recovery,
                Duration::from_secs(15),
            )
            .await
            .is_err()
            {
                restored = false;
            }
        }
        return if restored {
            Err(e)
        } else {
            Err(io::Error::other(format!(
                "gateway installation failed and rollback is incomplete: {e}"
            )))
        };
    }
    txn.commit();
    println!(
        "Gateway service active on 127.0.0.1:8088. Configured public URL: https://{}",
        options.domain
    );
    if enrollment == SecretState::Pending {
        println!("Finish administrator and TOTP setup in your browser at https://{}; the one-time setup token is stored privately at /var/lib/hmux-web/credentials.json.bootstrap.", options.domain);
    }
    if options.https == Https::External {
        println!("Configure the existing HTTPS reverse proxy to forward HTTP and WebSocket upgrades to 127.0.0.1:8088; forward Host, X-Real-IP and X-Forwarded-Proto=https; expose /connect over WSS. Keep the upstream loopback-only.");
    }
    if let Some(path) = &options.connection {
        println!("Private Home connection file created at {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let root = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = root.join(format!("hmux-e2e-gateway-{}", stamp().unwrap()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn options_reject_incomplete_and_unsafe_inputs() {
        for input in [
            a(&["--domain", "example.test", "--https", "managed"]),
            a(&[
                "--domain",
                "example.test",
                "--https",
                "managed",
                "--email",
                "a@example.test",
            ]),
            a(&[
                "--domain",
                "example.test",
                "--https",
                "external",
                "--install-packages",
            ]),
            a(&["--domain", "x;server.test", "--https", "external"]),
            a(&[
                "--domain",
                "example.test",
                "--https",
                "external",
                "--connection-file",
                "relative.json",
            ]),
            a(&[
                "--domain",
                "example.test",
                "--https",
                "external",
                "--https",
                "managed",
            ]),
        ] {
            assert!(parse(&input).is_err(), "{input:?}");
        }
        assert_eq!(
            parse(&a(&["--domain", "example.test", "--https", "external"]))
                .unwrap()
                .https,
            Https::External
        );
        assert_eq!(
            parse(&a(&[
                "--domain",
                "example.test",
                "--https",
                "managed",
                "--email",
                "a@example.test",
                "--accept-acme-terms"
            ]))
            .unwrap()
            .https,
            Https::Managed
        );
    }
    #[test]
    fn sites_keep_acme_only_until_certificate_and_proxy_websocket_after() {
        let challenge = challenge_site("example.test");
        assert!(
            challenge.contains("return 503")
                && !challenge.contains("proxy_pass")
                && !challenge.contains("listen 443")
        );
        let final_site = https_site("example.test");
        assert!(
            final_site.contains("listen 443 ssl")
                && final_site.contains("proxy_set_header Upgrade $http_upgrade")
                && final_site
                    .contains("ssl_certificate_key /etc/letsencrypt/live/example.test/privkey.pem")
        );
        assert!(!service_text().contains("User=root"));
    }
    #[test]
    fn managed_files_refuse_conflicts_and_roll_back() {
        let temp = Temp::new();
        let path = temp.0.join("site.conf");
        let backups = temp.0.join("backups");
        fs::create_dir(&backups).unwrap();
        fs::write(&path, "unmanaged").unwrap();
        let mut txn = ConfigTxn::new(backups.clone(), "one".into());
        assert!(txn
            .write(&path, challenge_site("example.test").as_bytes(), 0o644)
            .is_err());
        fs::remove_file(&path).unwrap();
        let old = challenge_site("old.test");
        fs::write(&path, &old).unwrap();
        txn.write(&path, challenge_site("new.test").as_bytes(), 0o644)
            .unwrap();
        assert!(fs::read_dir(&backups).unwrap().next().is_some());
        assert!(txn.rollback());
        assert_eq!(fs::read_to_string(&path).unwrap(), old);
    }
    #[test]
    fn pending_bootstrap_and_ready_pair_preserve_tokens() {
        use base64::Engine;
        let temp = Temp::new();
        let uid = rustix::process::geteuid().as_raw();
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([1_u8; 32]);
        let setup_token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([2_u8; 32]);
        assert_eq!(secret_state_at(&temp.0, uid).unwrap(), SecretState::Empty);
        let connector = temp.0.join("connector.token");
        let bootstrap = temp.0.join("credentials.json.bootstrap");
        fs::write(&connector, format!("{token}\n")).unwrap();
        fs::set_permissions(&connector, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(secret_state_at(&temp.0, uid).is_err());
        fs::write(&bootstrap, format!("{token}\n")).unwrap();
        fs::set_permissions(&bootstrap, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(secret_state_at(&temp.0, uid).is_err());
        fs::write(&bootstrap, format!("{setup_token}\n")).unwrap();
        assert_eq!(secret_state_at(&temp.0, uid).unwrap(), SecretState::Pending);
        let credentials = hmux_gateway::auth::Credentials {
            username: "test".into(),
            salt: vec![1; 32],
            hash: vec![2; 32],
            totp_secret: data_encoding::BASE32_NOPAD.encode(&[3_u8; 20]),
            ..Default::default()
        };
        fs::write(temp.0.join("credentials.json"), credentials.go_json()).unwrap();
        fs::set_permissions(
            temp.0.join("credentials.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(secret_state_at(&temp.0, uid).unwrap(), SecretState::Ready);
        let credential_path = temp.0.join("credentials.json");
        let mut oversized = credentials.go_json();
        oversized.push_str(&" ".repeat(4097));
        fs::write(&credential_path, oversized).unwrap();
        assert!(secret_state_at(&temp.0, uid).is_err());
        fs::write(&credential_path, credentials.go_json()).unwrap();
        fs::remove_file(bootstrap).unwrap();
        assert_eq!(secret_state_at(&temp.0, uid).unwrap(), SecretState::Ready);
        assert_eq!(fs::read_to_string(connector).unwrap(), format!("{token}\n"));
    }
    #[tokio::test]
    async fn readiness_probe_requires_gateway_status_from_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let read = socket.read(&mut request).await.unwrap();
            assert!(std::str::from_utf8(&request[..read])
                .unwrap()
                .contains("Host: example.test"));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"required\":true}").await.unwrap();
        });
        assert!(gateway_probe(address, "example.test").await.unwrap());
        server.await.unwrap();
        let stop = CancellationToken::new();
        stop.cancel();
        assert_eq!(
            await_gateway_ready_at(address, "example.test", &stop)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn staged_bundle_rejects_swapped_directory_and_changed_bytes_without_public_files() {
        let temp = Temp::new();
        let src = temp.0.join("bundle");
        let releases = temp.0.join("releases");
        let private = temp.0.join("private");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&releases).unwrap();
        fs::create_dir(&private).unwrap();
        fs::create_dir(src.join("web")).unwrap();
        fs::create_dir(src.join("licenses")).unwrap();
        for (name, content) in [
            ("hmux-web", "binary"),
            ("web/index.html", "web"),
            ("licenses/secret", "public fixture"),
            ("RELEASE", "runtime=rust\n"),
            ("THIRD_PARTY_NOTICES.md", "notices"),
        ] {
            fs::write(src.join(name), content).unwrap();
        }
        fs::write(private.join("secret"), "synthetic private content").unwrap();
        fs::set_permissions(src.join("hmux-web"), fs::Permissions::from_mode(0o755)).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let mut sums = String::new();
        for name in [
            "hmux-web",
            "web/index.html",
            "licenses/secret",
            "RELEASE",
            "THIRD_PARTY_NOTICES.md",
        ] {
            sums.push_str(&format!(
                "{}  ./{name}\n",
                hex_digest(&src.join(name), uid).unwrap()
            ));
        }
        fs::write(src.join("SHA256SUMS"), sums).unwrap();
        let bundle = verify_bundle(&src, uid).unwrap();
        fs::rename(src.join("licenses"), src.join("old-licenses")).unwrap();
        symlink(&private, src.join("licenses")).unwrap();
        assert!(stage_release_at(&releases, &bundle, "swapped", uid).is_err());
        assert!(!releases.join("swapped").exists());
        fs::remove_file(src.join("licenses")).unwrap();
        fs::rename(src.join("old-licenses"), src.join("licenses")).unwrap();
        fs::write(src.join("web/index.html"), "web with unverified growth").unwrap();
        assert!(stage_release_at(&releases, &bundle, "grown", uid).is_err());
        assert!(!releases.join("grown").exists());
        fs::write(src.join("web/index.html"), "abc").unwrap();
        assert!(stage_release_at(&releases, &bundle, "tampered", uid).is_err());
        assert!(!releases.join("tampered").exists());
        fs::write(src.join("web/index.html"), "web").unwrap();
        let good = stage_release_at(&releases, &bundle, "good", uid).unwrap();
        assert_eq!(
            fs::read_to_string(good.join("licenses/secret")).unwrap(),
            "public fixture"
        );
        assert_eq!(
            fs::metadata(good.join("licenses/secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn bundle_manifest_rejects_tamper_and_symlink() {
        let temp = Temp::new();
        let src = temp.0.join("bundle");
        fs::create_dir(&src).unwrap();
        fs::create_dir(src.join("web")).unwrap();
        for (name, content) in [
            ("hmux-web", "binary"),
            ("web/index.html", "web"),
            ("RELEASE", "runtime=rust\n"),
            ("THIRD_PARTY_NOTICES.md", "notices"),
        ] {
            fs::write(src.join(name), content).unwrap();
        }
        fs::set_permissions(src.join("hmux-web"), fs::Permissions::from_mode(0o755)).unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let mut sums = String::new();
        for name in [
            "hmux-web",
            "web/index.html",
            "RELEASE",
            "THIRD_PARTY_NOTICES.md",
        ] {
            sums.push_str(&format!(
                "{}  ./{name}\n",
                hex_digest(&src.join(name), uid).unwrap()
            ));
        }
        fs::write(src.join("SHA256SUMS"), sums).unwrap();
        assert_eq!(verify_bundle(&src, uid).unwrap().files.len(), 4);
        fs::write(src.join("web/index.html"), "tampered").unwrap();
        assert!(verify_bundle(&src, uid).is_err());
        fs::remove_file(src.join("web/index.html")).unwrap();
        symlink("/etc/passwd", src.join("web/index.html")).unwrap();
        assert!(verify_bundle(&src, uid).is_err());
    }
}
