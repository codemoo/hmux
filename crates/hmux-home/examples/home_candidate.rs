//! Experimental standalone Home. This is deliberately outside installation and
//! deployment paths until provider/session/service parity is complete.
use hmux_home::runtime::{HomeRuntime, Options};
use std::{io, path::Path, process::ExitCode};
use tokio_util::sync::CancellationToken;

const HELP: &str = "Experimental HMux Home candidate\n\
Usage: home_candidate --experimental-home --url wss://hmux.example/connect \\\n  --token-file /absolute/private/connector.token --config /absolute/private/home.toml \\\n  [--tmux /absolute/path/to/tmux] [--tmux-socket /absolute/private/test.socket]\n\
Optional uploads: --staging-root /absolute/private/hmux/staged-files-v1\n\
Incomplete candidate: use disposable test state and tmux only. No service installation.\n";

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = Options::parse(args)
        .map_err(io::Error::other)
        .and_then(|options| hmux_core::runtime::run_process(run(options))?);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("home_candidate: {error}");
            ExitCode::FAILURE
        }
    }
}
async fn run(options: Options) -> io::Result<()> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME unavailable"))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let runtime =
        HomeRuntime::prepare(options, Path::new(&home), &path).map_err(io::Error::other)?;
    let mut lifecycle = runtime.lifecycle();
    let stop = CancellationToken::new();
    let serving = runtime.run(stop.clone());
    tokio::pin!(serving);
    loop {
        tokio::select! {
            biased;
            _ = terminate.recv() => { stop.cancel(); return serving.await.map_err(io::Error::other); },
            _ = interrupt.recv() => { stop.cancel(); return serving.await.map_err(io::Error::other); },
            result = &mut serving => return result.map_err(io::Error::other),
            state = lifecycle.changed() => {
                if let Some(state) = state { eprintln!("Home candidate: {state:?}"); }
            }
        }
    }
}
