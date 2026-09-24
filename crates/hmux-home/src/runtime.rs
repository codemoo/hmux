//! Home entrypoint assembly. Preparation resolves tmux once, opens private inputs
//! and takes the existing singleton before starting connected work.
use crate::{
    catalog::{TmuxCatalogReader, TmuxSocket},
    config,
    connector::{self, LifecycleObserver, Prepared},
    dial::Endpoint,
    filestage::Store,
    inspection::Inspector,
    sessions,
};
use hmux_core::command::CommandRunner;
use std::{
    ffi::{OsStr, OsString},
    fmt, fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Arguments,
    Endpoint,
    Token,
    Config,
    Tmux,
    Staging,
    Connector(connector::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Arguments => "invalid Home candidate arguments; see --help",
            Self::Endpoint => "Home endpoint must be wss://host/connect",
            Self::Token => "connector token unavailable or invalid",
            Self::Config => "Home configuration unavailable or invalid",
            Self::Staging => "upload staging root unavailable or unsafe",
            Self::Tmux => "tmux executable or socket unavailable or invalid",
            Self::Connector(_) => {
                "Home connector unavailable; check state permissions and duplicate processes"
            }
        })
    }
}
impl std::error::Error for Error {}

pub struct Options {
    endpoint: String,
    token_file: PathBuf,
    config_file: Option<PathBuf>,
    tmux: Option<PathBuf>,
    socket: Option<TmuxSocket>,
    staging_root: Option<PathBuf>,
}
impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Options([redacted])")
    }
}
impl Options {
    /// Installed `hmux-web connect` contract. An explicit config must exist;
    /// omitting it retains home.toml/client.toml/default lookup. Uploads use the
    /// same host cache as the Go runtime for rolling replacement.
    pub fn connect(
        endpoint: String,
        token_file: PathBuf,
        config_file: Option<PathBuf>,
        home: &Path,
        cache_home: Option<&OsStr>,
    ) -> Result<Self, Error> {
        Endpoint::parse(&endpoint).map_err(|_| Error::Endpoint)?;
        let cache = default_cache(home, cache_home)?;
        Ok(Self {
            endpoint,
            token_file,
            config_file,
            tmux: None,
            socket: None,
            staging_root: Some(cache.join("hmux/staged-files-v1")),
        })
    }

    /// The incomplete candidate requires opt-in and explicit private files. Its
    /// two tmux flags are test/host overrides, never shell command fragments.
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, Error> {
        let mut args = args.into_iter();
        let mut experimental = false;
        let mut endpoint = None;
        let mut token = None;
        let mut config = None;
        let mut tmux = None;
        let mut socket = None;
        let mut staging_root = None;
        for _ in 0..8 {
            let Some(flag) = args.next() else { break };
            if flag == "--experimental-home" && !experimental {
                experimental = true;
                continue;
            }
            let value = args.next().ok_or(Error::Arguments)?;
            if value.is_empty() || value.as_bytes().len() > 4096 || value.as_bytes().contains(&0) {
                return Err(Error::Arguments);
            }
            match flag.to_str() {
                Some("--url") if endpoint.is_none() => {
                    endpoint = Some(value.into_string().map_err(|_| Error::Arguments)?);
                }
                Some("--token-file") if token.is_none() => token = Some(PathBuf::from(value)),
                Some("--config") if config.is_none() => config = Some(PathBuf::from(value)),
                Some("--tmux") if tmux.is_none() => tmux = Some(PathBuf::from(value)),
                Some("--tmux-socket") if socket.is_none() => {
                    socket = Some(TmuxSocket::Path(PathBuf::from(value)));
                }
                Some("--staging-root") if staging_root.is_none() => {
                    staging_root = Some(PathBuf::from(value))
                }
                _ => return Err(Error::Arguments),
            }
        }
        if args.next().is_some() || !experimental {
            return Err(Error::Arguments);
        }
        let options = Self {
            endpoint: endpoint.ok_or(Error::Arguments)?,
            token_file: token.ok_or(Error::Arguments)?,
            config_file: Some(config.ok_or(Error::Arguments)?),
            tmux,
            socket,
            staging_root,
        };
        if !options.token_file.is_absolute()
            || !options
                .config_file
                .as_ref()
                .is_some_and(|p| p.is_absolute())
        {
            return Err(Error::Arguments);
        }
        Endpoint::parse(&options.endpoint).map_err(|_| Error::Endpoint)?;
        Ok(options)
    }
}

pub struct HomeRuntime {
    prepared: Prepared,
    catalog: TmuxCatalogReader,
    runner: CommandRunner,
}
impl fmt::Debug for HomeRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HomeRuntime([redacted])")
    }
}
impl HomeRuntime {
    /// Synchronous startup I/O, before async serving. No tmux command or network
    /// action occurs here. Invalid inputs fail before creating the state lock.
    pub fn prepare(options: Options, home: &Path, path: &OsStr) -> Result<Self, Error> {
        let token = hmux_core::token::load(&options.token_file).map_err(|_| Error::Token)?;
        let config = match &options.config_file {
            Some(path) => config::load_home_required(path, home),
            None => config::load_home(None, home),
        }
        .map_err(|_| Error::Config)?;
        let tmux = match options.tmux {
            Some(program) if program.is_absolute() && executable(&program) => program,
            Some(_) => return Err(Error::Tmux),
            None => find_tmux(path)?,
        };
        let catalog =
            TmuxCatalogReader::new(tmux.clone(), options.socket.clone(), Duration::from_secs(5))
                .map_err(|_| Error::Tmux)?;
        let runner = CommandRunner::new(2).map_err(|_| Error::Tmux)?;
        let session_context = sessions::Context::new(
            home.to_path_buf(),
            path.to_os_string(),
            std::env::var_os("SHELL").unwrap_or_default(),
        )
        .map_err(|_| Error::Config)?;
        let store = options
            .staging_root
            .map(Store::open)
            .transpose()
            .map_err(|_| Error::Staging)?;
        let config_state_dir = config.state_dir.clone();
        let providers = crate::providers::ProviderService::new(
            crate::providers::ProviderEnv::for_home(
                home.to_path_buf(),
                path.to_os_string(),
                config.inventory_path.clone(),
                tmux.clone(),
            ),
            CommandRunner::new(3).map_err(|_| Error::Config)?,
        )
        .map_err(|_| Error::Config)?;
        let mut prepared =
            Prepared::prepare(config, &options.endpoint, &token).map_err(Error::Connector)?;
        let workspace =
            crate::workspace::Workspace::open(&config_state_dir).map_err(|_| Error::Config)?;
        prepared = prepared.with_providers(Arc::new(providers));
        prepared = prepared
            .with_session_context(Arc::new(session_context))
            .with_metrics(Arc::new(crate::metrics::Collector::native()));
        if let Ok(usage) = crate::usage_config::Options::capture(home, path) {
            prepared = prepared.with_usage(usage);
        }
        let mut resolver: crate::recovery::Resolver = Arc::new(|panes| {
            Box::pin(async move {
                if panes.is_empty() {
                    Ok(Default::default())
                } else {
                    Err(crate::recovery::Error::Unavailable)
                }
            })
        });
        if let Some(ps) = find_optional_tool(path, "ps", &["/bin/ps", "/usr/bin/ps"]) {
            let lsof = find_optional_tool(path, "lsof", &["/usr/sbin/lsof", "/usr/bin/lsof"]);
            if let Ok(inspector) = Inspector::new(home.to_path_buf(), ps, lsof) {
                let inspector = Arc::new(inspector);
                resolver = crate::recovery_binding::resolver(inspector.clone());
                prepared = prepared
                    .with_inspector(inspector)
                    .with_completion_notifications();
            }
        }
        let recovery = crate::recovery::Store::new(
            config_state_dir,
            tmux,
            options.socket,
            CommandRunner::new(2).map_err(|_| Error::Config)?,
            resolver,
        )
        .map_err(|_| Error::Config)?;
        prepared = prepared
            .with_workspace(workspace.with_recovery(recovery.clone()))
            .with_recovery(recovery);
        if let Some(store) = store {
            prepared = prepared.with_upload_store(Arc::new(store));
        }
        Ok(Self {
            prepared,
            catalog,
            runner,
        })
    }
    pub fn lifecycle(&self) -> LifecycleObserver {
        self.prepared.lifecycle()
    }
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), Error> {
        self.prepared
            .run(self.catalog, self.runner, shutdown)
            .await
            .map_err(Error::Connector)
    }
}
fn default_cache(home: &Path, cache_home: Option<&OsStr>) -> Result<PathBuf, Error> {
    if !home.is_absolute() {
        return Err(Error::Config);
    }
    let cache = if cfg!(target_os = "macos") {
        home.join("Library/Caches")
    } else {
        cache_home
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
    };
    if !cache.is_absolute() {
        return Err(Error::Staging);
    }
    Ok(cache)
}
fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}
// Resolve once at startup; missing inspection tools must not stop the connector.
// An untrusted relative PATH entry is never turned into a command to execute.
fn find_optional_tool(path: &OsStr, name: &str, fallbacks: &[&str]) -> Option<PathBuf> {
    if path.as_bytes().len() > 64 * 1024 || path.as_bytes().contains(&0) {
        return None;
    }
    for directory in std::env::split_paths(path).take(256) {
        if directory.is_absolute() {
            let candidate = directory.join(name);
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    fallbacks.iter().map(PathBuf::from).find(|p| executable(p))
}
fn find_tmux(path: &OsStr) -> Result<PathBuf, Error> {
    if path.is_empty() || path.as_bytes().len() > 64 * 1024 || path.as_bytes().contains(&0) {
        return Err(Error::Tmux);
    }
    for (index, directory) in std::env::split_paths(path).enumerate() {
        if index >= 256 {
            return Err(Error::Tmux);
        }
        let candidate = directory.join("tmux");
        if executable(&candidate) {
            // Match Go's ErrDot: do not execute a discovered relative command.
            return if candidate.is_absolute() {
                Ok(candidate)
            } else {
                Err(Error::Tmux)
            };
        }
    }
    Err(Error::Tmux)
}
