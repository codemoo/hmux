//! Synthetic current-state compatibility bridge; not a production entrypoint.
use hmux_core::command::CommandRunner;
use hmux_home::recovery::{ResumeReference, Store};
use hmux_model::Catalog;
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::PathBuf, sync::Arc};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    root: PathBuf,
    tmux: PathBuf,
    boot_id: String,
    operation: String,
    #[serde(default)]
    socket_name: Option<String>,
    #[serde(default)]
    reference: Option<ResumeReference>,
    #[serde(default)]
    catalog: Catalog,
}

fn main() {
    if run().is_err() {
        eprintln!("synthetic recovery operation failed");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(256 * 1024 + 1)
        .read_to_end(&mut raw)?;
    if raw.len() > 256 * 1024 {
        return Err("input limit".into());
    }
    let mut q: Request = serde_json::from_slice(&raw)?;
    let reference = q.reference;
    let resolver = Arc::new(move |pids: Vec<i32>| {
        let reference = reference.clone();
        Box::pin(async move {
            Ok(pids
                .into_iter()
                .filter_map(|pid| reference.clone().map(|r| (pid, r)))
                .collect::<BTreeMap<_, _>>())
        }) as hmux_home::recovery::ResolveFuture
    });
    let boot = q.boot_id;
    let socket = q.socket_name.map(hmux_home::catalog::TmuxSocket::Name);
    let store = Store::new(q.root, q.tmux, socket, CommandRunner::new(2)?, resolver)?
        .with_boot_id(Arc::new(move || Ok(boot.clone())));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match q.operation.as_str() {
            "save" => store.save().await,
            "sync" => store.sync().await,
            "restore" => store.restore().await,
            "apply" => Ok(()),
            _ => Err(hmux_home::recovery::Error::Invalid),
        }
    })?;
    store.apply(&mut q.catalog)?;
    serde_json::to_writer(std::io::stdout(), &q.catalog)?;
    Ok(())
}
