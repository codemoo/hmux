//! Isolated authentication test server, not an hmux-web replacement.
use hmux_core::PrivateDir;
use hmux_gateway::{auth_store::AuthStore, http_auth::Gateway, http_boundary::loopback_address};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const HELP: &str = r"Experimental HMux authentication candidate
Usage: auth_gateway --experimental-auth-only --origin https://hmux.example
  --credentials /absolute/synthetic/credentials.json --token-file /absolute/synthetic/connector.token
  [--listen 127.0.0.1:8088]
Only login/account/session authentication routes are implemented. All other protected routes return 503.
Use isolated synthetic state; never run concurrently with Go on the same state.
";

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result =
        options(arguments).and_then(|options| hmux_core::runtime::run_process(run(options))?);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("auth_gateway: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    origin: String,
    credentials: PathBuf,
    token: PathBuf,
    listen: String,
}
fn options(arguments: Vec<String>) -> io::Result<Options> {
    let mut args = arguments.into_iter();
    let mut values = HashMap::new();
    let mut experimental = false;
    while let Some(flag) = args.next() {
        if flag == "--experimental-auth-only" && !experimental {
            experimental = true;
            continue;
        }
        if !["--origin", "--credentials", "--token-file", "--listen"].contains(&flag.as_str()) {
            return Err(io::Error::other("unknown or duplicate flag; see --help"));
        }
        let value = args
            .next()
            .ok_or_else(|| io::Error::other("missing flag value"))?;
        if value.starts_with("--") || values.insert(flag, value).is_some() {
            return Err(io::Error::other("missing or duplicate flag value"));
        }
    }
    if !experimental {
        return Err(io::Error::other(
            "--experimental-auth-only is required; this is not a production server",
        ));
    }
    let origin = values
        .remove("--origin")
        .ok_or_else(|| io::Error::other("--origin is required"))?;
    let credentials = absolute(
        values
            .remove("--credentials")
            .ok_or_else(|| io::Error::other("--credentials is required"))?,
    )?;
    let token = absolute(
        values
            .remove("--token-file")
            .ok_or_else(|| io::Error::other("--token-file is required"))?,
    )?;
    let listen = values
        .remove("--listen")
        .unwrap_or_else(|| "127.0.0.1:8088".into());
    loopback_address(&listen)?;
    Ok(Options {
        origin,
        credentials,
        token,
        listen,
    })
}
fn absolute(value: String) -> io::Result<PathBuf> {
    let path = Path::new(&value);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Err(io::Error::other("candidate state paths must be absolute"))
    }
}

async fn run(options: Options) -> io::Result<()> {
    // One bounded startup read; no recurring collector or daemon is introduced.
    let token = tokio::task::spawn_blocking(move || {
        let parent = options
            .token
            .parent()
            .ok_or_else(|| io::Error::other("missing token parent"))?;
        let name = options
            .token
            .file_name()
            .ok_or_else(|| io::Error::other("missing token basename"))?;
        let raw = PrivateDir::open(parent)?.read_private(name, 256)?;
        String::from_utf8(raw)
            .map(|s| s.trim().to_owned())
            .map_err(|_| io::Error::other("invalid connector token"))
    })
    .await
    .map_err(|_| io::Error::other("token reader failed"))??;
    let auth = Arc::new(AuthStore::open(options.credentials).await?);
    let gateway = Arc::new(Gateway::new(&options.origin, &token, auth).map_err(io::Error::other)?);
    let listener = TcpListener::bind(loopback_address(&options.listen)?).await?;
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let signals = tokio::spawn(async move {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        stop.cancel();
    });
    eprintln!(
        "Experimental auth-only listener ready at {}; other routes are unavailable.",
        listener.local_addr()?
    );
    let result = gateway.serve(listener, shutdown).await;
    signals.abort();
    let _ = signals.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(value: &[&str]) -> Vec<String> {
        value.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn candidate_requires_explicit_role_private_paths_and_loopback() {
        assert!(options(args(&["--origin", "https://hmux.example"])).is_err());
        let base = [
            "--experimental-auth-only",
            "--origin",
            "https://hmux.example",
            "--credentials",
            "/synthetic/credentials.json",
            "--token-file",
            "/synthetic/connector.token",
        ];
        assert!(options(args(&base)).is_ok());
        let mut public = args(&base);
        public.extend(args(&["--listen", "0.0.0.0:8088"]));
        assert!(options(public).is_err());
        let mut duplicate = args(&base);
        duplicate.extend(args(&["--origin", "https://other.example"]));
        assert!(options(duplicate).is_err());
        let mut relative = args(&base);
        relative[4] = "relative.json".into();
        assert!(options(relative).is_err());
    }
}
