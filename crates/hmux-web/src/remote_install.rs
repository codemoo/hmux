//! SSH is installation transport only. Home runtime remains a native connector.
use crate::{args::invalid, enroll::line_prompt, install_process, locale};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub fn valid_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('@').count() <= 2
        && value.split('@').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn remote_arguments(
    bundle: &str,
    role: &str,
    export: &str,
    language: locale::Language,
) -> Vec<String> {
    let mut arguments = vec![
        format!("{bundle}/hmux-web"),
        "install".into(),
        "--local".into(),
        "--lang".into(),
        language.code().into(),
        "--role".into(),
        role.into(),
        "--source-dir".into(),
        bundle.into(),
    ];
    if role != "home" {
        arguments.extend(["--connection-output".into(), export.into()]);
    }
    arguments
}
fn trusted_program(path: &str) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(invalid("a trusted system SSH client is required"));
    }
    Ok(())
}
fn ssh(host: &str, tty: bool, script: &str) -> Command {
    let mut command = Command::new("/usr/bin/ssh");
    command.args([
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        if tty { "-tt" } else { "-T" },
        "--",
        host,
        script,
    ]);
    command
}
async fn call(
    host: &str,
    tty: bool,
    script: &str,
    capture: bool,
    stop: &CancellationToken,
) -> io::Result<Vec<u8>> {
    install_process::run(
        &mut ssh(host, tty, script),
        capture,
        Duration::from_secs(if tty { 3600 } else { 120 }),
        stop,
    )
    .await
}
fn platform(raw: &[u8]) -> io::Result<&'static str> {
    match std::str::from_utf8(raw).unwrap_or("").trim() {
        "Linux\nx86_64" => Ok("linux-amd64"),
        "Linux\naarch64" | "Linux\narm64" => Ok("linux-arm64"),
        "Darwin\nx86_64" => Ok("darwin-amd64"),
        "Darwin\narm64" => Ok("darwin-arm64"),
        _ => Err(invalid("remote host must be supported macOS/Linux on arm64 or x86_64; ensure non-interactive SSH startup is quiet")),
    }
}
fn metadata(path: &Path) -> io::Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink()
        || meta.mode() & 0o022 != 0
        || !(meta.uid() == 0 || meta.uid() == rustix::process::getuid().as_raw())
        || (meta.is_file() && meta.nlink() != 1)
    {
        return Err(invalid(
            "remote installation bundle contains an unsafe file",
        ));
    }
    Ok(meta)
}
fn read_bounded(path: &Path, max: u64) -> io::Result<Vec<u8>> {
    let meta = metadata(path)?;
    if !meta.is_file() || meta.len() > max {
        return Err(invalid("installation bundle file exceeds limit"));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(invalid("installation bundle file exceeds limit"));
    }
    Ok(bytes)
}
fn matches_platform(source: &Path, expected: &str) -> bool {
    read_bounded(&source.join("RELEASE"), 4096)
        .ok()
        .is_some_and(|data| {
            String::from_utf8_lossy(&data)
                .lines()
                .any(|line| line == format!("platform={expected}"))
        })
}
fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/@+-".contains(&b))
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
        && value.split('/').all(|p| !matches!(p, "" | "." | ".."))
}
fn collect_files(
    root: &Path,
    relative: &Path,
    files: &mut BTreeSet<String>,
    bytes: &mut u64,
    entries: &mut usize,
) -> io::Result<()> {
    if relative.components().count() > 24 {
        return Err(invalid("bundle nesting exceeds limit"));
    }
    for entry in fs::read_dir(root.join(relative))? {
        let path = relative.join(entry?.file_name());
        let name = path
            .to_str()
            .ok_or_else(|| invalid("bundle paths must be UTF-8"))?;
        *entries += 1;
        if *entries > 8192 || !safe_name(name) {
            return Err(invalid("bundle paths exceed limit or are unsafe"));
        }
        let meta = metadata(&root.join(&path))?;
        if meta.is_dir() {
            collect_files(root, &path, files, bytes, entries)?;
        } else if meta.is_file() {
            *bytes = bytes.saturating_add(meta.len());
            if *bytes > 512 << 20 {
                return Err(invalid("bundle size exceeds limit"));
            }
            files.insert(name.into());
        } else {
            return Err(invalid("bundle contains a special file"));
        }
    }
    Ok(())
}
fn validate_bundle(source: &Path, expected: &str) -> io::Result<()> {
    hmux_core::PrivateDir::open_existing_trusted(source)?;
    if !matches_platform(source, expected) {
        return Err(invalid("bundle platform does not match the remote host"));
    }
    let mut actual = BTreeSet::new();
    collect_files(source, Path::new(""), &mut actual, &mut 0, &mut 0)?;
    let manifest = read_bounded(&source.join("SHA256SUMS"), 2 << 20)?;
    let manifest =
        std::str::from_utf8(&manifest).map_err(|_| invalid("invalid bundle manifest"))?;
    let mut listed = BTreeSet::from(["SHA256SUMS".to_string()]);
    for line in manifest.lines() {
        let (hash, name) = line
            .split_once("  ")
            .ok_or_else(|| invalid("invalid bundle manifest"))?;
        let name = name.strip_prefix("./").unwrap_or(name);
        if hash.len() != 64
            || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            || !safe_name(name)
            || !listed.insert(name.to_string())
        {
            return Err(invalid("invalid bundle manifest"));
        }
        let mut file = fs::File::open(source.join(name))?;
        let mut digest = Sha256::new();
        let mut buffer = [0; 64 << 10];
        let mut total = 0u64;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > 512 << 20 {
                return Err(invalid("bundle file exceeds limit"));
            }
            digest.update(&buffer[..n]);
        }
        if format!("{:x}", digest.finalize()) != hash.to_ascii_lowercase() {
            return Err(invalid("installation bundle checksum mismatch"));
        }
    }
    if actual != listed
        || !actual.contains("hmux-web")
        || !actual.contains("hmux-agent")
        || !actual.contains("web/index.html")
    {
        return Err(invalid("installation bundle manifest is incomplete"));
    }
    for name in ["hmux-web", "hmux-agent"] {
        if metadata(&source.join(name))?.mode() & 0o111 == 0 {
            return Err(invalid("bundle binary is not executable"));
        }
    }
    Ok(())
}
async fn copy(
    host: &str,
    source: &Path,
    destination: &str,
    directory: bool,
    stop: &CancellationToken,
) -> io::Result<()> {
    let mut command = Command::new("/usr/bin/scp");
    command.args([
        "-p",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ConnectTimeout=15",
    ]);
    if directory {
        command.arg("-r");
    }
    command
        .arg("--")
        .arg(source)
        .arg(format!("{host}:{destination}"));
    install_process::run(&mut command, false, Duration::from_secs(600), stop).await?;
    Ok(())
}

pub async fn run(
    host: &str,
    role: &str,
    source: PathBuf,
    connection: Option<PathBuf>,
    workspace: Option<OsString>,
    stop: CancellationToken,
) -> io::Result<()> {
    if !valid_host(host) {
        return Err(invalid(
            "use a valid SSH host alias or user@hostname; configure ports/keys in SSH config",
        ));
    }
    trusted_program("/usr/bin/ssh")?;
    trusted_program("/usr/bin/scp")?;
    println!(
        "\n{}",
        locale::tr("Checking SSH target. Its host key must already be verified in known_hosts.")
    );
    let remote = platform(&call(host, false, "uname -s; uname -m", true, &stop).await?)?;
    if remote.starts_with("darwin") && role != "home" {
        return Err(invalid(
            "macOS remote hosts support Home only; Gateway requires Linux/systemd",
        ));
    }
    println!("{} {remote}", locale::tr("Remote platform:"));
    let source = if matches_platform(&source, remote) {
        source
    } else {
        let sibling = source
            .parent()
            .unwrap_or(Path::new("/"))
            .join(format!("web-{remote}"));
        if matches_platform(&sibling, remote) {
            sibling
        } else {
            let stop = stop.clone();
            tokio::task::spawn_blocking(move || {
                let answer = line_prompt(
                    &stop,
                    &format!(
                        "{} {remote} {}",
                        locale::tr("Local path to the extracted"),
                        locale::tr("bundle:")
                    ),
                )?;
                crate::args::absolute(Path::new(&answer).as_os_str())
            })
            .await
            .map_err(io::Error::other)??
        }
    };
    let candidate = source.clone();
    tokio::task::spawn_blocking(move || validate_bundle(&candidate, remote))
        .await
        .map_err(io::Error::other)??;
    if let Some(ref file) = connection {
        crate::pairing::ConnectionFile::read(file)?;
    }
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let temp = if remote.starts_with("darwin") {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let staging = format!("{temp}/hmux-install-{:032x}", u128::from_le_bytes(nonce));
    let export = format!("{staging}/home-connection.json");
    call(
        host,
        false,
        &format!("umask 077 && mkdir {}", quote(&staging)),
        false,
        &stop,
    )
    .await?;
    let result = async {
        let bundle = format!("{staging}/bundle");
        println!("{}", locale::tr("Transferring verified installation files…"));
        copy(host, &source, &bundle, true, &stop).await?;
        let mut arguments = remote_arguments(&bundle, role, &export, locale::current());
        if let Some(file) = connection {
            let destination = format!("{staging}/connection.json");
            copy(host, &file, &destination, false, &stop).await?;
            arguments.extend(["--connection-file".into(), destination]);
        }
        if let Some(workspace) = workspace {
            arguments.extend(["--workspace-dir".into(), workspace.into_string().map_err(|_|invalid("workspace path must be UTF-8"))?]);
        }
        let arguments = arguments.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        let script = format!("cd {} && if [ -x /usr/bin/sha256sum ]; then /usr/bin/sha256sum -c SHA256SUMS >/dev/null; elif [ -x /usr/bin/shasum ]; then /usr/bin/shasum -a 256 -c SHA256SUMS >/dev/null; else exit 1; fi && exec {arguments}", quote(&bundle));
        if locale::korean() { println!("{host}에서 설치를 시작합니다. 이후 설정은 모두 해당 호스트에 적용됩니다."); }
        else { println!("Starting installer on {host}. All following setup choices apply to that host."); }
        call(host, true, &script, false, &stop).await?;
        Ok(())
    }.await;
    let mut transferred = Ok(());
    if role != "home" && !stop.is_cancelled() {
        // The remote Gateway may have succeeded even if optional Home setup
        // failed. Keep its new connector handoff recoverable on either host.
        if call(
            host,
            false,
            &format!("test -f {}", quote(&export)),
            false,
            &stop,
        )
        .await
        .is_ok()
        {
            transferred = retrieve(host, &export, &stop).await;
        }
    }
    if result.is_err() || transferred.is_err() {
        if locale::korean() {
            eprintln!("원격 설치 파일을 {staging}에 보관했습니다. 설치된 역할은 제거하지 않았습니다. 오류를 확인한 뒤 다시 시도하세요.");
        } else {
            eprintln!("Remote installation files retained at {staging}. The installed roles, if any, were not removed. Check the error before retrying.");
        }
        return result.and(transferred);
    }
    // Remove only the create-exclusive nonce directory, never installed files.
    let cleanup = call(
        host,
        false,
        &format!("rm -rf -- {}", quote(&staging)),
        false,
        &CancellationToken::new(),
    )
    .await;
    if cleanup.is_err() {
        if locale::korean() {
            eprintln!("원격 임시 파일이 {staging}에 남아 있습니다. 대상 장치를 확인한 뒤 해당 디렉터리를 삭제하세요.");
        } else {
            eprintln!("Remote staging files were retained at {staging}; remove that directory after checking the target.");
        }
    }
    result
}

async fn retrieve(host: &str, remote: &str, stop: &CancellationToken) -> io::Result<()> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("HOME unavailable"))?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let parent = home
        .join(".config/hmux/connections")
        .join(format!("remote-{:032x}", u128::from_le_bytes(nonce)));
    hmux_core::PrivateDir::open_or_create_trusted(&parent)?;
    let local = parent.join("home-connection.json");
    let mut command = Command::new("/usr/bin/scp");
    command
        .args([
            "-p",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=15",
            "--",
        ])
        .arg(format!("{host}:{remote}"))
        .arg(&local);
    install_process::run(&mut command, false, Duration::from_secs(120), stop).await?;
    crate::pairing::ConnectionFile::read(&local)?;
    println!(
        "\n{} {}",
        locale::tr("Private Home connection file saved locally:"),
        local.display()
    );
    println!("{}", locale::tr("Use it with install --role home --connection-file FILE on the Home host. It grants connector access; do not publish it."));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn remote_shell_boundary_rejects_options_and_injection() {
        for host in ["box", "me@hmux.example", "192.0.2.1"] {
            assert!(valid_host(host));
        }
        for host in [
            "-oProxyCommand=id",
            "a;id",
            "me@host/path",
            "user@a@b",
            "a\nb",
            "@host",
            "u@-host",
        ] {
            assert!(!valid_host(host));
        }
        assert_eq!(quote("/work/it's $(id)"), "'/work/it'\\''s $(id)'");
        assert_eq!(platform(b"Linux\nx86_64\n").unwrap(), "linux-amd64");
        assert!(platform(b"Welcome\nLinux\nx86_64").is_err());
        for path in ["../x", "/absolute", "a//b", "a/./b", "a\nx", "a b"] {
            assert!(!safe_name(path));
        }
    }

    #[test]
    fn remote_installer_argv_carries_allowlisted_locale() {
        let argv = remote_arguments(
            "/tmp/bundle",
            "home",
            "/tmp/connection",
            locale::Language::Korean,
        );
        assert_eq!(argv[3..5], ["--lang", "ko"]);
        assert_eq!(argv[5..7], ["--role", "home"]);
        assert!(!argv.iter().any(|a| a == "--connection-output"));
        let script_args = argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ");
        assert!(script_args.contains("'--lang' 'ko'"));
    }

    #[test]
    fn transferred_bundle_requires_matching_platform_complete_manifest_and_trusted_files() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-remote-{:032x}",
            u128::from_le_bytes(nonce)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("web")).unwrap();
        let files = [
            ("RELEASE", "platform=linux-amd64\nruntime=rust\n"),
            ("hmux-web", "synthetic-web"),
            ("hmux-agent", "synthetic-agent"),
            ("web/index.html", "web"),
        ];
        let mut manifest = String::new();
        for (name, contents) in files {
            fs::write(root.join(name), contents).unwrap();
            fs::set_permissions(
                root.join(name),
                fs::Permissions::from_mode(if name.starts_with("hmux-") {
                    0o700
                } else {
                    0o600
                }),
            )
            .unwrap();
            manifest.push_str(&format!(
                "{:x}  ./{name}\n",
                Sha256::digest(contents.as_bytes())
            ));
        }
        fs::write(root.join("SHA256SUMS"), &manifest).unwrap();
        assert!(validate_bundle(&root, "linux-amd64").is_ok());
        assert!(validate_bundle(&root, "darwin-arm64").is_err());
        fs::write(root.join("extra"), "not listed").unwrap();
        assert!(validate_bundle(&root, "linux-amd64").is_err());
        fs::remove_file(root.join("extra")).unwrap();
        fs::write(root.join("web/index.html"), "tampered").unwrap();
        assert!(validate_bundle(&root, "linux-amd64").is_err());
        fs::remove_file(root.join("web/index.html")).unwrap();
        std::os::unix::fs::symlink("../hmux-web", root.join("web/index.html")).unwrap();
        assert!(validate_bundle(&root, "linux-amd64").is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
