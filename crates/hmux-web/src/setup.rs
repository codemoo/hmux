//! One entry point for local roles. Privileged Gateway work stays in its Linux installer.
use crate::{
    args::{absolute, invalid},
    enroll::line_prompt,
};
use std::{
    ffi::OsString,
    io::{self, IsTerminal},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const USAGE: &str = "usage: hmux-web install [--role all|gateway|home] [--local | --remote SSH_HOST] [--source-dir DIR] [--connection-file FILE] [--workspace-dir DIR]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    All,
    Gateway,
    Home,
}
impl Role {
    fn parse(value: &str) -> io::Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "gateway" => Ok(Self::Gateway),
            "home" => Ok(Self::Home),
            _ => Err(invalid("role must be all, gateway or home")),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Gateway => "gateway",
            Self::Home => "home",
        }
    }
    fn check(self, linux: bool, root: bool) -> io::Result<()> {
        if self != Self::Home && !linux {
            return Err(invalid("Gateway provisioning requires Linux with systemd; use Home on macOS and install Gateway on your Linux server"));
        }
        if self != Self::Gateway && root {
            return Err(invalid("run Home or combined installation as your normal tmux/provider user, without sudo; Gateway elevation is handled separately"));
        }
        Ok(())
    }
}

#[derive(Default)]
struct Options {
    role: Option<Role>,
    source: Option<PathBuf>,
    connection: Option<PathBuf>,
    workspace: Option<OsString>,
    local: bool,
    remote: Option<String>,
    output: Option<PathBuf>,
}

fn parse(args: &[OsString]) -> io::Result<Options> {
    if args.len() > 16 || args.iter().map(|a| a.len()).sum::<usize>() > 16384 {
        return Err(invalid("installer arguments exceed limit"));
    }
    let mut result = Options::default();
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let raw = flag.to_str().ok_or_else(|| invalid(USAGE))?;
        let (flag, inline) = raw
            .split_once('=')
            .map_or((raw, None), |(k, v)| (k, Some(v)));
        if flag == "--local" {
            if inline.is_some() || result.local {
                return Err(invalid("duplicate or invalid --local"));
            }
            result.local = true;
            continue;
        }
        let value = match inline {
            Some(v) => v,
            None => args
                .next()
                .and_then(|a| a.to_str())
                .ok_or_else(|| invalid(USAGE))?,
        };
        if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(invalid("invalid installer option"));
        }
        match flag {
            "--remote" if result.remote.is_none() && crate::remote_install::valid_host(value) => {
                result.remote = Some(value.into())
            }
            "--role" if result.role.is_none() => result.role = Some(Role::parse(value)?),
            "--source-dir" if result.source.is_none() => result.source = Some(value.into()),
            "--connection-file" if result.connection.is_none() => {
                result.connection = Some(value.into())
            }
            "--connection-output" if result.output.is_none() => result.output = Some(value.into()),
            "--workspace-dir" if result.workspace.is_none() => {
                result.workspace = Some(value.into())
            }
            _ => return Err(invalid(USAGE)),
        }
    }
    if result.local && result.remote.is_some() {
        return Err(invalid("--local and --remote cannot be combined"));
    }
    if result.output.is_some()
        && (result.connection.is_some()
            || result.role == Some(Role::Home)
            || result.remote.is_some())
    {
        return Err(invalid("--connection-output requires a local Gateway role"));
    }
    if result.connection.is_some() {
        if result.role.is_some_and(|r| r != Role::Home) {
            return Err(invalid("--connection-file is for Home-only installation"));
        }
        result.role = Some(Role::Home);
    }
    if result.workspace.is_some() && result.role == Some(Role::Gateway) {
        return Err(invalid(
            "--workspace-dir is for Home or combined installation",
        ));
    }
    Ok(result)
}

fn yes(stop: &CancellationToken, prompt: &str) -> io::Result<bool> {
    loop {
        match line_prompt(stop, prompt)?.to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => println!("Enter y or n."),
        }
    }
}
fn domain(value: &str) -> bool {
    value.len() <= 253
        && value.contains('.')
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
fn expand(value: &Path, home: &Path) -> io::Result<PathBuf> {
    let value = if value == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(tail) = value.strip_prefix("~/") {
        home.join(tail)
    } else {
        value.to_path_buf()
    };
    absolute(value.as_os_str())
}

struct Plan {
    role: Role,
    gateway: Vec<OsString>,
    connection: Option<PathBuf>,
    proceed: bool,
}
fn collect(opts: &Options, home: &Path, stop: &CancellationToken) -> io::Result<Plan> {
    println!("\nHMux / Install\nLow memory, web terminal for AI agents.\n");
    let linux = cfg!(target_os = "linux");
    let root = rustix::process::geteuid().as_raw() == 0;
    let role = opts.role.expect("installation role selected");
    role.check(linux, root)?;
    if role == Role::Home {
        let connection = if let Some(ref path) = opts.connection {
            Some(expand(path, home)?)
        } else {
            let value = line_prompt(
                stop,
                "Gateway connection file [Enter to enter connection details later]: ",
            )?;
            if value.is_empty() {
                None
            } else {
                Some(expand(Path::new(&value), home)?)
            }
        };
        if let Some(ref path) = connection {
            crate::pairing::ConnectionFile::read(path)?;
        }
        return Ok(Plan {
            role,
            gateway: vec![],
            connection,
            proceed: true,
        });
    }
    if opts.workspace.is_some() && role == Role::Gateway {
        return Err(invalid("workspace configuration requires a Home role"));
    }
    let host = loop {
        let host = line_prompt(stop, "Gateway domain (for example hmux.example.com): ")?
            .to_ascii_lowercase();
        if domain(&host) {
            break host;
        }
        println!("Enter a DNS hostname without https://, a path or a port.");
    };
    println!("\nHTTPS setup:\n  1  Configure Nginx + Let's Encrypt on this server\n  2  Use an existing HTTPS reverse proxy\n");
    let managed = loop {
        match line_prompt(stop, "HTTPS mode [1/2]: ")?.as_str() {
            "1" => break true,
            "2" => break false,
            _ => println!("Choose 1 or 2."),
        }
    };
    let mut gateway = vec![
        "--domain".into(),
        host.into(),
        "--https".into(),
        if managed {
            "managed".into()
        } else {
            "external".into()
        },
    ];
    if managed {
        let email = loop {
            let value = line_prompt(stop, "Email for certificate renewal notices: ")?;
            if value.len() <= 254
                && value.is_ascii()
                && !value.bytes().any(|b| b.is_ascii_whitespace())
                && value.split_once('@').is_some_and(|(name, host)| {
                    !name.is_empty() && !name.starts_with('-') && domain(host)
                })
            {
                break value;
            }
            println!("Enter a valid email address.");
        };
        gateway.extend(["--email".into(), email.into()]);
        println!("The domain must point here, with ports 80 and 443 reachable. Existing unrelated Nginx sites are retained.");
        println!("Let's Encrypt subscriber terms: https://letsencrypt.org/repository/");
        if !yes(stop, "Accept the certificate subscriber terms? [y/N]: ")? {
            return Ok(Plan {
                role,
                gateway,
                connection: None,
                proceed: false,
            });
        }
        gateway.push("--accept-acme-terms".into());
        if yes(
            stop,
            "Install missing Nginx/Certbot packages using apt? [y/N]: ",
        )? {
            gateway.push("--install-packages".into());
        }
    } else {
        println!("Your existing HTTPS proxy must forward HTTP and WebSocket upgrades to 127.0.0.1:8088 on this server. HMux will not change that proxy.");
    }
    println!("\nGateway installation creates a dedicated service account, private credentials and a systemd service.");
    if role == Role::All {
        println!("Home setup follows under your current user, with the Gateway connection filled in automatically.");
    }
    let proceed = yes(stop, "Install Gateway with these settings? [y/N]: ")?;
    Ok(Plan {
        role,
        gateway,
        connection: None,
        proceed,
    })
}

fn target(mut opts: Options, stop: &CancellationToken) -> io::Result<Options> {
    if opts.role.is_none() {
        println!("\nHMux / Install\n\nChoose the role for the target machine:\n  1  Gateway + Home  / one Linux server\n  2  Gateway only    / Linux HTTPS entry point\n  3  Home only       / macOS or Linux agent host\n");
        opts.role = Some(loop {
            match line_prompt(stop, "Installation role [1/2/3]: ")?.as_str() {
                "1" => break Role::All,
                "2" => break Role::Gateway,
                "3" => break Role::Home,
                _ => println!("Choose 1, 2 or 3."),
            }
        });
    }
    if !opts.local && opts.remote.is_none() {
        println!("\nInstall on:\n  1  This machine\n  2  A remote server over SSH\n");
        loop {
            match line_prompt(stop, "Installation target [1/2]: ")?.as_str() {
                "1" => {
                    opts.local = true;
                    break;
                }
                "2" => {
                    let host = line_prompt(stop, "SSH host alias or user@hostname: ")?;
                    if crate::remote_install::valid_host(&host) {
                        opts.remote = Some(host);
                        break;
                    }
                    println!("Use an SSH host alias or user@hostname. Configure ports and keys in SSH config.");
                }
                _ => println!("Choose 1 or 2."),
            }
        }
    }
    if opts.workspace.is_some() && opts.role == Some(Role::Gateway) {
        return Err(invalid("workspace configuration requires a Home role"));
    }
    if opts.output.is_some() && (opts.role == Some(Role::Home) || opts.remote.is_some()) {
        return Err(invalid("--connection-output requires a local Gateway role"));
    }
    Ok(opts)
}

pub async fn run(args: &[OsString], stop: CancellationToken) -> io::Result<()> {
    if args == ["--help"] || args == ["-h"] {
        println!("{USAGE}\n\nChoose Gateway + Home, Gateway only, or Home only.\nGateway provisioning requires Linux/systemd and administrative access.\nHome supports macOS/launchd and Linux/systemd user services.\nRun as the normal Home user; only Gateway provisioning uses sudo.\nChoose this machine or an SSH target. Remote installs need its matching native bundle and a verified SSH host key.\n\nExamples:\n  hmux-web install\n  hmux-web install --role home --connection-file /PRIVATE/home-connection.json\n\nThis guide needs a terminal. For automation, use install-home or install-gateway with explicit options.");
        return Ok(());
    }
    let opts = parse(args)?;
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(invalid("install requires an interactive terminal; use install-home or install-gateway for automation"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("HOME unavailable"))?;
    let executable = std::env::current_exe()?;
    let source = expand(
        opts.source.as_deref().unwrap_or(
            executable
                .parent()
                .ok_or_else(|| invalid("installer location unavailable"))?,
        ),
        &home,
    )?;
    let cancel = stop.clone();
    let opts = tokio::task::spawn_blocking(move || target(opts, &cancel))
        .await
        .map_err(io::Error::other)??;
    if let Some(ref host) = opts.remote {
        let connection = opts
            .connection
            .as_ref()
            .map(|p| expand(p, &home))
            .transpose()?;
        return crate::remote_install::run(
            host,
            opts.role.expect("role selected").name(),
            source,
            connection,
            opts.workspace,
            stop,
        )
        .await;
    }
    opts.role.expect("role selected").check(
        cfg!(target_os = "linux"),
        rustix::process::geteuid().as_raw() == 0,
    )?;
    if opts.role != Some(Role::Gateway) {
        crate::install::validate_candidate(&source)?;
    }
    let workspace = opts.workspace.clone();
    let output = opts.output.as_ref().map(|p| expand(p, &home)).transpose()?;
    let owned_home = home.clone();
    let cancel = stop.clone();
    let plan = tokio::task::spawn_blocking(move || collect(&opts, &owned_home, &cancel))
        .await
        .map_err(io::Error::other)??;
    if !plan.proceed {
        println!("Installation cancelled. No Gateway or Home configuration was changed.");
        return Ok(());
    }
    if stop.is_cancelled() {
        return Err(invalid("installation cancelled"));
    }
    let connection = if plan.role != Role::Home {
        let mut random = [0; 8];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let parent = home
            .join(".config/hmux/connections")
            .join(format!("setup-{:016x}", u64::from_le_bytes(random)));
        hmux_core::PrivateDir::open_or_create_trusted(&parent)?;
        let path = output.unwrap_or_else(|| parent.join("home-connection.json"));
        let mut args = plan.gateway;
        args.extend([
            "--source-dir".into(),
            source.as_os_str().into(),
            "--connection-file".into(),
            path.as_os_str().into(),
        ]);
        gateway(&executable, &args, &stop).await?;
        crate::pairing::ConnectionFile::read(&path)?;
        println!("\nPrivate Home connection file: {}", path.display());
        println!("This file grants access to the Home connector. Transfer it only over a trusted private channel; never publish it.");
        if plan.role == Role::Gateway {
            println!("On your Home machine, run the bundle's hmux-web install --role home --connection-file /PRIVATE/home-connection.json");
            return Ok(());
        }
        Some(path)
    } else {
        // Preserve imported pairing information before an SSH upload staging
        // directory is removed, including when automatic startup is declined.
        plan.connection
            .map(|path| {
                let connection = crate::pairing::ConnectionFile::read(&path)?;
                let retained = connection.retain(&home)?;
                println!("Private connection file retained at {}", retained.display());
                Ok::<_, io::Error>(retained)
            })
            .transpose()?
    };
    let mut home_args = vec![
        "--guided".into(),
        "--source-dir".into(),
        source.into_os_string(),
    ];
    if let Some(ref path) = connection {
        home_args.extend(["--connection-file".into(), path.as_os_str().into()]);
    }
    if let Some(workspace) = workspace {
        home_args.extend(["--workspace-dir".into(), workspace]);
    }
    let result = crate::install::run(&home_args, stop).await;
    if result.is_err() && plan.role == Role::All {
        eprintln!("Gateway setup finished; Home setup did not finish. The Gateway and private connection file were retained. Resume with install --role home --connection-file using that file.");
    }
    result
}

async fn gateway(executable: &Path, args: &[OsString], stop: &CancellationToken) -> io::Result<()> {
    let root = rustix::process::geteuid().as_raw() == 0;
    let mut cmd = if root {
        tokio::process::Command::new(executable)
    } else {
        let metadata = std::fs::symlink_metadata("/usr/bin/sudo")?;
        if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(invalid(
                "trusted /usr/bin/sudo is required for Gateway provisioning",
            ));
        }
        let mut cmd = tokio::process::Command::new("/usr/bin/sudo");
        cmd.arg("--").arg(executable);
        cmd
    };
    cmd.arg("install-gateway").args(args);
    crate::install_process::run(&mut cmd, false, Duration::from_secs(1800), stop).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }
    #[test]
    fn roles_enforce_host_and_privilege_boundaries() {
        assert!(Role::All.check(true, false).is_ok());
        assert!(Role::Gateway.check(true, true).is_ok());
        assert!(Role::Home.check(false, false).is_ok());
        assert!(Role::All.check(false, false).is_err());
        assert!(Role::All.check(true, true).is_err());
        assert!(Role::Home.check(true, true).is_err());
    }
    #[test]
    fn options_route_connection_files_to_home_without_mixing_gateway_settings() {
        assert_eq!(
            parse(&args(&["--connection-file", "/private/connection.json"]))
                .unwrap()
                .role,
            Some(Role::Home)
        );
        for raw in [
            args(&["--role", "gateway", "--connection-file", "/private/file"]),
            args(&["--role", "gateway", "--workspace-dir", "/work"]),
            args(&["--role", "all", "--role", "home"]),
            args(&["--sudo"]),
        ] {
            assert!(parse(&raw).is_err());
        }
        for name in ["hmux.example.com", "xn--test.example"] {
            assert!(domain(name));
        }
        for name in [
            "https://hmux.example",
            "localhost",
            "-bad.example",
            "hmux.example/path",
            "hmux.example;id",
            "a..example",
        ] {
            assert!(!domain(name));
        }
    }
}
