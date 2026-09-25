//! Native Home installation. The old Python launcher can delegate to this command.
use crate::args::{absolute, invalid};
use hmux_core::command::{CommandRunner, CommandSpec};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
#[derive(Default, Debug)]
struct Options {
    source: Option<PathBuf>,
    bin: Option<PathBuf>,
    config: Option<PathBuf>,
    workspace: Option<OsString>,
    binaries_only: bool,
    enable_service: bool,
    endpoint: Option<OsString>,
    token: Option<PathBuf>,
    guided: bool,
    connection_file: Option<PathBuf>,
}
const USAGE: &str = "usage: hmux-web install-home [--guided] [--connection-file FILE] [--source-dir DIR] [--bin-dir DIR] [--config-dir DIR] [--workspace-dir DIR] [--binaries-only | --enable-service [--url wss://host/connect --token-file FILE]]";
fn parse(args: &[OsString]) -> io::Result<Options> {
    if args.len() > 64 || args.iter().map(|v| v.len()).sum::<usize>() > 65536 {
        return Err(invalid("installation arguments exceed limit"));
    }
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].to_str().ok_or_else(|| invalid(USAGE))?;
        let (key, inline) = raw
            .split_once('=')
            .map_or((raw, None), |(k, v)| (k, Some(OsString::from(v))));
        match key {
            "--binaries-only" | "--enable-service" | "--guided" => {
                if inline.is_some() {
                    return Err(invalid("installation switches do not accept a value"));
                }
                match key {
                    "--binaries-only" => options.binaries_only = true,
                    "--enable-service" => options.enable_service = true,
                    _ => options.guided = true,
                }
            }
            _ => {
                let value = if let Some(ref inline) = inline {
                    inline
                } else {
                    index += 1;
                    args.get(index).ok_or_else(|| invalid(USAGE))?
                };
                if value.is_empty() {
                    return Err(invalid(USAGE));
                }
                match key {
                    "--source-dir" => options.source = Some(value.into()),
                    "--bin-dir" => options.bin = Some(value.into()),
                    "--config-dir" => options.config = Some(value.into()),
                    "--workspace-dir" => options.workspace = Some(value.clone()),
                    "--url" => options.endpoint = Some(value.clone()),
                    "--token-file" => options.token = Some(value.into()),
                    "--connection-file" => options.connection_file = Some(value.into()),
                    _ => return Err(invalid(USAGE)),
                }
            }
        }
        index += 1;
    }
    if options.binaries_only && options.enable_service {
        return Err(invalid(
            "--binaries-only cannot be combined with --enable-service",
        ));
    }
    if options.binaries_only && options.guided {
        return Err(invalid("--guided cannot be combined with --binaries-only"));
    }
    if options.endpoint.is_some() != options.token.is_some() {
        return Err(invalid("--url and --token-file must be supplied together"));
    }
    if options.endpoint.is_some() && !options.enable_service {
        return Err(invalid("--url and --token-file require --enable-service"));
    }
    if options.connection_file.is_some()
        && (options.binaries_only
            || options.endpoint.is_some()
            || !(options.guided || options.enable_service))
    {
        return Err(invalid("--connection-file requires --guided or --enable-service and cannot combine with --binaries-only or explicit connection options"));
    }
    Ok(options)
}
fn resolved(path: &Path, home: &Path) -> io::Result<PathBuf> {
    let expanded = if path == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(tail) = path.strip_prefix("~/") {
        home.join(tail)
    } else {
        path.to_path_buf()
    };
    absolute(expanded.as_os_str())
}
fn active(stop: &CancellationToken) -> io::Result<()> {
    if stop.is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "installation cancelled",
        ))
    } else {
        Ok(())
    }
}
pub(crate) fn validate_candidate(source: &Path) -> io::Result<()> {
    // The helper runs before publication, so retain the installer's trusted
    // directory and owner-controlled executable checks at this earlier point.
    hmux_core::PrivateDir::open_existing_trusted(source)?;
    for name in ["hmux-web", "hmux-agent"] {
        let metadata = std::fs::symlink_metadata(source.join(name))?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.nlink() != 1
            || metadata.len() == 0
            || metadata.len() > 256 * 1024 * 1024
            || metadata.mode() & 0o022 != 0
            || metadata.permissions().mode() & 0o111 == 0
        {
            return Err(invalid(
                "candidate binary must be an owner-controlled executable",
            ));
        }
    }
    Ok(())
}
async fn command(binary: &Path, args: Vec<OsString>, stop: &CancellationToken) -> io::Result<()> {
    active(stop)?;
    let runner = CommandRunner::new(1).map_err(io::Error::other)?;
    let spec = CommandSpec::new(binary.as_os_str(), 128 << 10, Duration::from_secs(120)).args(args);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let running = runner.run_cancelable(spec, rx);
    tokio::pin!(running);
    let result = tokio::select! {
        value=&mut running=>value,
        ()=stop.cancelled()=> {let _=tx.send(());running.await},
    }
    .map_err(io::Error::other)?;
    io::stdout().write_all(&result.stdout)
}
pub async fn run(args: &[OsString], stop: CancellationToken) -> io::Result<()> {
    if args == ["--help"] || args == ["-h"] {
        println!("{USAGE}\n\nInstall the native Home connector and helper.\n\n  --connection-file Import a private Gateway connection JSON with --guided\n                    or --enable-service; existing different tokens are refused\n  --guided          Walk through workspace and connection setup\n  --enable-service  Register automatic startup; adopt a running connector\n                    when --url and --token-file are omitted\n  --binaries-only   Update executables without changing configuration\n\nExamples:\n  hmux-web install-home --guided\n  hmux-web install-home --enable-service --url wss://hmux.example/connect --token-file ~/.config/hmux/web/connector.token\n\nAutomatic startup is opt-in. Existing workspace paths are preserved.");
        return Ok(());
    }
    if rustix::process::geteuid().as_raw() == 0 {
        return Err(invalid(
            "run Home installation as the tmux/provider user without sudo",
        ));
    }
    let mut options = parse(args)?;
    if options.guided && !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(invalid(
            "--guided requires an interactive terminal; use explicit flags for automation",
        ));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("HOME unavailable"))?;
    let source = resolved(
        &options.source.unwrap_or(
            std::env::current_exe()?
                .parent()
                .ok_or_else(|| invalid("installer location unavailable"))?
                .to_path_buf(),
        ),
        &home,
    )?;
    let bin = resolved(&options.bin.unwrap_or(home.join(".local/bin")), &home)?;
    let config = resolved(&options.config.unwrap_or(home.join(".config/hmux")), &home)?;
    let ui = crate::install_ui::Display::new();
    ui.welcome(options.binaries_only);
    ui.step(1, "Check installation files");
    validate_candidate(&source)?;
    let pairing_path = options
        .connection_file
        .as_ref()
        .map(|p| resolved(p, &home))
        .transpose()?;
    let pairing = pairing_path
        .as_ref()
        .map(|p| crate::pairing::ConnectionFile::read(p))
        .transpose()?;
    if let Some(ref file) = pairing {
        file.check_target(&config)?;
        println!("  Private Gateway connection file validated. Token contents stay hidden.");
    }
    if !options.binaries_only {
        let mut existing = false;
        for name in ["home.toml", "client.toml", "inventory.toml"] {
            match std::fs::symlink_metadata(config.join(name)) {
                Ok(_) => existing = true,
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e),
            }
        }
        ui.step(2, "Choose your workspace");
        if existing && options.workspace.is_none() {
            println!("  Existing configuration found. Keeping your workspace paths.");
        }
        if !existing && options.workspace.is_none() && io::stdin().is_terminal() {
            let stop = stop.clone();
            options.workspace = Some(
                tokio::task::spawn_blocking(move || crate::enroll::workspace_prompt(&stop))
                    .await
                    .map_err(io::Error::other)??
                    .into(),
            );
        }
        if options.guided {
            ui.dependencies();
            ui.step(3, "Connect your Home");
            let stop = stop.clone();
            let enable_service = options.enable_service;
            let imported_connection = pairing.is_some();
            let explicit_connection = options.endpoint.is_some() || imported_connection;
            let home = home.clone();
            let config = config.clone();
            let choice = tokio::task::spawn_blocking(move || {
                crate::install_ui::connection(
                    &stop,
                    enable_service,
                    explicit_connection,
                    imported_connection,
                    &home,
                    &config,
                )
            })
            .await
            .map_err(io::Error::other)??;
            options.enable_service = choice.enable_service;
            if let Some((endpoint, token)) = choice.connection {
                options.endpoint = Some(endpoint.into());
                options.token = Some(token);
            }
        }
    }
    if options.binaries_only {
        active(&stop)?;
        let target = bin.clone();
        tokio::task::spawn_blocking(move || hmux_install::install(&source, &target))
            .await
            .map_err(io::Error::other)??;
        active(&stop)?;
        println!("Installed hmux-web and hmux-agent");
        ui.complete(&bin, &config, false, true);
        return Ok(());
    }
    let mut setup = vec![
        "setup-home".into(),
        "--config-dir".into(),
        config.as_os_str().into(),
    ];
    if let Some(workspace) = options.workspace {
        setup.extend(["--workspace-dir".into(), workspace]);
    }
    ui.step(if options.guided { 4 } else { 3 }, "Install Home");
    command(&source.join("hmux-agent"), setup, &stop).await?;
    println!("Home configured; existing paths are preserved unless --workspace-dir is supplied.");
    let enable_service = options.enable_service;
    if enable_service {
        active(&stop)?;
        if let Some(ref file) = pairing {
            options.endpoint = Some(file.endpoint.clone().into());
            options.token = Some(file.install_token(&config)?);
        }
        let binary = bin.join("hmux-web");
        let mut service = vec![
            "service".into(),
            "install".into(),
            "--binary".into(),
            binary.as_os_str().into(),
        ];
        if let (Some(endpoint), Some(token)) = (options.endpoint, options.token) {
            let config_path = if config.join("home.toml").try_exists()? {
                config.join("home.toml")
            } else {
                config.join("client.toml")
            };
            service.extend([
                "--url".into(),
                endpoint,
                "--token-file".into(),
                resolved(&token, &home)?.into_os_string(),
                "--config".into(),
                config_path.into_os_string(),
            ]);
        } else {
            service.push("--from-running".into());
        }
        let args = service
            .into_iter()
            .skip(1)
            .map(|s| {
                s.into_string()
                    .map_err(|_| invalid("service paths must be UTF-8"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let output = hmux_service::run_with_bundle(&args, stop, source, bin.clone()).await?;
        println!("Installed hmux-web and hmux-agent");
        print!("{output}");
    } else {
        active(&stop)?;
        let target = bin.clone();
        tokio::task::spawn_blocking(move || hmux_install::install(&source, &target))
            .await
            .map_err(io::Error::other)??;
        active(&stop)?;
        println!("Installed hmux-web and hmux-agent");
    }
    ui.complete(&bin, &config, enable_service, false);
    if !enable_service && pairing_path.is_some() {
        println!("Keep your private connection file. Pass --connection-file to a later guided installation to connect without retyping its settings.");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incomplete_service_options_before_mutation() {
        for args in [
            vec!["--url", "wss://example/connect"],
            vec!["--token-file", "/token"],
            vec!["--enable-service", "--binaries-only"],
            vec!["--unknown", "x"],
            vec!["--guided", "--binaries-only"],
            vec!["--enable-service=false"],
            vec!["--guided=no"],
            vec!["--connection-file", "/private/connection.json"],
            vec![
                "--guided",
                "--binaries-only",
                "--connection-file",
                "/private/connection.json",
            ],
        ] {
            assert!(parse(&args.into_iter().map(Into::into).collect::<Vec<_>>()).is_err());
        }
        assert!(parse(&["--binaries-only".into()]).unwrap().binaries_only);
    }
}
