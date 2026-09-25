//! Native gateway, Home connector and user-service entrypoint.
mod args;
mod enroll;
mod install;
mod install_ui;
mod logging;
use args::{absolute, invalid, Options};
use hmux_gateway::runtime::{GatewayRuntime, Options as GatewayOptions};
use hmux_home::runtime::{HomeRuntime, Options as HomeOptions};
use std::{ffi::OsString, io, path::Path, process::ExitCode};
use tokio_util::sync::CancellationToken;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("{}", args::USAGE);
        return ExitCode::SUCCESS;
    }
    let result = hmux_core::runtime::run_process(run(args)).and_then(|r| r);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
async fn run(arguments: Vec<OsString>) -> io::Result<()> {
    let Some(command) = arguments.first().and_then(|s| s.to_str()) else {
        return Err(invalid(args::USAGE));
    };
    if !matches!(
        command,
        "init" | "serve" | "connect" | "service" | "install-home"
    ) {
        return Err(invalid("unknown command"));
    }
    // Signal registration precedes any owner, startup I/O, password prompt or
    // service mutation. Cancellation always joins the command being cancelled.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let stop = CancellationToken::new();
    let running = dispatch(command, &arguments[1..], stop.clone());
    tokio::pin!(running);
    tokio::select! {
        biased;
        _ = terminate.recv() => { stop.cancel(); running.await },
        _ = interrupt.recv() => { stop.cancel(); running.await },
        result = &mut running => result,
    }
}
async fn dispatch(
    command: &str,
    arguments: &[OsString],
    stop: CancellationToken,
) -> io::Result<()> {
    if command == "install-home" {
        return install::run(arguments, stop).await;
    }
    if command == "service" {
        let args = arguments
            .iter()
            .map(|s| {
                s.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("service arguments must be UTF-8"))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let output = hmux_service::run(&args, stop).await?;
        print!("{output}");
        return Ok(());
    }
    let options = args::parse(arguments)?;
    match command {
        "init" => tokio::task::spawn_blocking(move || enroll::initialize(options, stop))
            .await
            .map_err(io::Error::other)?,
        "serve" => serve(options, stop).await,
        "connect" => connect(options, stop).await,
        _ => Err(invalid("unknown command")),
    }
}
async fn serve(opts: Options, stop: CancellationToken) -> io::Result<()> {
    let listen = hmux_gateway::http_boundary::loopback_address(&opts.listen)?;
    let assets = absolute(&opts.assets)?;
    if !assets
        .join("index.html")
        .metadata()
        .is_ok_and(|m| m.is_file())
    {
        return Err(invalid(
            "built web assets missing; run npm ci && npm run build in web/",
        ));
    }
    let options = GatewayOptions {
        origin: opts.origin,
        credentials: absolute(&opts.credentials)?,
        token_file: absolute(&opts.token)?,
        assets,
        listen,
    };
    // The opening future owns partially initialized private stores. Never drop
    // it on a signal; drain resulting owners without accepting requests.
    let mut log_path = options.credentials.as_os_str().to_os_string();
    log_path.push(".transport.log");
    let output = hmux_core::log::Log::open(Path::new(&log_path))
        .map_err(|_| io::Error::other("gateway transport log unavailable"))?;
    let log = logging::Log::start(output);
    let events = log.sender();
    events.send(logging::Event::Message("Gateway starting"));
    let runtime = match GatewayRuntime::open_reported(options, Some(events.reporter())).await {
        Ok(runtime) => runtime,
        Err(error) => {
            events.send(logging::Event::Message("Gateway startup failed"));
            let _ = log.shutdown().await;
            return Err(error);
        }
    };
    if stop.is_cancelled() {
        runtime.shutdown().await;
        return log.shutdown().await;
    }
    let address = match runtime.local_addr() {
        Ok(a) => a,
        Err(e) => {
            runtime.shutdown().await;
            let _ = log.shutdown().await;
            return Err(e);
        }
    };
    println!("HMux web listening on {address} behind HTTPS");
    let result = runtime.serve(stop).await;
    events.send(logging::Event::Message("Gateway stopped"));
    let flushed = log.shutdown().await;
    result.and(flushed)
}
async fn connect(opts: Options, stop: CancellationToken) -> io::Result<()> {
    let writer: Box<dyn io::Write + Send> = if opts.log.is_empty() {
        Box::new(io::stderr())
    } else {
        Box::new(hmux_core::log::Log::open(&absolute(&opts.log)?)?)
    };
    let log = logging::Log::start(writer);
    let events = log.sender();
    events.send(logging::Event::Message("Starting Home connector"));
    let result = connect_logged(opts, stop, &events).await;
    if result.is_err() {
        events.send(logging::Event::Message(
            "Home connector failed; check configuration and duplicate processes",
        ));
    }
    let flushed = log.shutdown().await;
    result.and(flushed)
}
async fn connect_logged(
    opts: Options,
    stop: CancellationToken,
    events: &logging::Sender,
) -> io::Result<()> {
    let home = std::env::var_os("HOME").ok_or_else(|| invalid("HOME unavailable"))?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let cache = std::env::var_os("XDG_CACHE_HOME");
    let config = (!opts.config.is_empty())
        .then(|| absolute(&opts.config))
        .transpose()?;
    let options = HomeOptions::connect(
        opts.endpoint,
        absolute(&opts.token)?,
        config,
        Path::new(&home),
        cache.as_deref(),
    )
    .map_err(io::Error::other)?;
    let runtime = HomeRuntime::prepare(options, Path::new(&home), &path)
        .map_err(io::Error::other)?
        .with_reporter(events.home_reporter());
    let mut lifecycle = runtime.lifecycle();
    println!("Home connector running; Ctrl-C disconnects web access without ending tmux work.");
    let serving = runtime.run(stop);
    tokio::pin!(serving);
    let mut observing = true;
    loop {
        tokio::select! {
            result = &mut serving => return result.map_err(io::Error::other),
            state = lifecycle.changed(), if observing => match state {
                Some(state) => events.send(logging::Event::Home(state)),
                None => observing = false,
            }
        }
    }
}
