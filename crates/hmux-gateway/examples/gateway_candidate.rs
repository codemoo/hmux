//! Separate experimental gateway role. Go Home and production installation
//! remain unchanged; this executable requires explicit private inputs.
use hmux_gateway::runtime::{GatewayRuntime, Options};
use std::{io, process::ExitCode};
use tokio_util::sync::CancellationToken;

const HELP: &str = r"Experimental HMux gateway candidate
Usage: gateway_candidate --experimental-gateway --origin https://hmux.example \
  --credentials /absolute/private/credentials.json --token-file /absolute/private/connector.token \
  --assets /absolute/public/web [--listen 127.0.0.1:8088]
This is an isolated candidate gateway, not an installed Home connector.
";

fn main() -> ExitCode {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments == ["--help"] || arguments == ["-h"] {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = Options::parse(arguments)
        .and_then(|options| hmux_core::runtime::run_process(run(options))?);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("gateway_candidate: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> io::Result<()> {
    // Register both handlers before the first owner or blocking trust load starts.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let opening = GatewayRuntime::open(options);
    tokio::pin!(opening);
    // Never drop a partially initialized startup future: let its in-flight
    // initialization finish, then drain the resulting owners without serving.
    let (runtime, cancelled) = tokio::select! {
        biased;
        _ = terminate.recv() => (opening.await?, true),
        _ = interrupt.recv() => (opening.await?, true),
        result = &mut opening => (result?, false),
    };
    if cancelled {
        runtime.shutdown().await;
        return Ok(());
    }
    let address = match runtime.local_addr() {
        Ok(address) => address,
        Err(error) => {
            runtime.shutdown().await;
            return Err(error);
        }
    };
    let shutdown = CancellationToken::new();
    let serving = runtime.serve(shutdown.clone());
    tokio::pin!(serving);
    eprintln!("Experimental gateway listener ready at {address}");
    tokio::select! {
        result = &mut serving => result,
        _ = interrupt.recv() => { shutdown.cancel(); serving.await },
        _ = terminate.recv() => { shutdown.cancel(); serving.await },
    }
}
