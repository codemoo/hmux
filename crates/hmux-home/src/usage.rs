//! One usage collector per Home connector lifetime. Slow sources run independently;
//! reconnecting peers subscribe to a single latest-value channel, never start work.
use crate::{
    dial, peer, usage_activity, usage_config, usage_credentials, usage_http, usage_lb, usage_oauth,
    usage_sources,
};
use chrono::{DateTime, Utc};
use hmux_protocol::{
    protobuf::{types as p, Negotiated},
    snapshots,
    transport::Sender,
};
use hmux_usage::{
    activity::ActivitySnapshot, cswap, model::format_time, sources, transport, Provider, Snapshot,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{interval, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

const REFRESH: Duration = Duration::from_secs(60);
const SCAN: Duration = Duration::from_secs(5);
const PUBLISH: Duration = Duration::from_secs(1);
const HEARTBEAT_TICKS: u8 = 10;

#[derive(Clone)]
pub struct Latest {
    pub claude: Arc<Snapshot>,
    pub codex: Arc<Snapshot>,
}
pub type Receiver = watch::Receiver<Option<Latest>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Config,
    Encoding,
    Worker,
}

/// Coalesced configuration/authentication refresh; no task or queue per caller.
#[derive(Clone)]
pub struct RefreshHandle(watch::Sender<u64>);
impl RefreshHandle {
    pub fn request(&self) {
        self.0.send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }
}

pub struct Collector {
    refresh: RefreshHandle,
    latest: Receiver,
    stop: CancellationToken,
    task: Option<JoinHandle<Result<(), Error>>>,
}
impl Collector {
    pub fn start(
        options: usage_config::Options,
        client: dial::Client,
        parent: &CancellationToken,
    ) -> Self {
        let stop = parent.child_token();
        let (sender, latest) = watch::channel(None);
        let (refresh, refresh_rx) = watch::channel(0);
        let task = tokio::spawn(run(
            options,
            usage_http::Client::new(client),
            sender,
            refresh_rx,
            stop.clone(),
        ));
        Self {
            refresh: RefreshHandle(refresh),
            latest,
            stop,
            task: Some(task),
        }
    }
    pub fn refresh_handle(&self) -> RefreshHandle {
        self.refresh.clone()
    }
    pub fn subscribe(&self) -> Receiver {
        self.latest.clone()
    }
    /// Normal shutdown joins the collector and all cooperative source futures.
    /// Native readers keep their process-wide admission until their real exit.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        self.stop.cancel();
        self.task
            .take()
            .expect("owned collector")
            .await
            .map_err(|_| Error::Worker)?
    }
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

#[derive(Clone)]
struct Lb {
    selected: bool,
    snapshot: Snapshot,
}
struct Inputs {
    claude: watch::Receiver<Snapshot>,
    codex: watch::Receiver<Snapshot>,
    swap: watch::Receiver<Option<cswap::Parsed>>,
    lb: watch::Receiver<Lb>,
    activity: watch::Receiver<Option<usage_activity::Sample>>,
}
fn degraded(provider: Provider, now: DateTime<Utc>) -> Snapshot {
    Snapshot::degraded(provider, 0, now, "networkError")
}
async fn run(
    options: usage_config::Options,
    client: usage_http::Client,
    output: watch::Sender<Option<Latest>>,
    refresh: watch::Receiver<u64>,
    stop: CancellationToken,
) -> Result<(), Error> {
    let _cancel_on_drop = stop.clone().drop_guard();
    let now = Utc::now();
    let store = usage_credentials::Store::new(&options.home).map_err(|_| Error::Config)?;
    let (claude_tx, claude) = watch::channel(degraded(Provider::Claude, now));
    let (codex_tx, codex) = watch::channel(degraded(Provider::Codex, now));
    let (swap_tx, swap) = watch::channel(None);
    let (lb_tx, lb) = watch::channel(Lb {
        selected: false,
        snapshot: usage_lb::unavailable(0, now, "networkError"),
    });
    let (activity_tx, activity) = watch::channel(None);
    let claude_owner = usage_oauth::Owner::new(Provider::Claude, store.clone(), client.clone());
    let codex_owner = usage_oauth::Owner::new(Provider::Codex, store, client.clone());
    let swap_owner = options
        .cswap
        .then(|| usage_sources::Cswap::new(options.home.clone(), options.path.clone()).ok())
        .flatten();
    let account_owner = options
        .accounts
        .clone()
        .and_then(|path| usage_sources::Accounts::new(path).ok());
    let activity_owner = options
        .activity
        .clone()
        .and_then(|options| usage_activity::Reader::new(options, now).ok());
    // No detached per-source tasks. All futures belong to this connector task;
    // a refresh waiting on I/O never blocks publication, catalog or terminals.
    let (_, _, _, _, _, result) = tokio::join!(
        oauth(claude_owner, claude_tx, refresh.clone(), &stop),
        oauth(codex_owner, codex_tx, refresh.clone(), &stop),
        swap_source(swap_owner, swap_tx, refresh.clone(), &stop),
        lb_source(&options, client, account_owner, lb_tx, refresh, &stop),
        activity_source(activity_owner, activity_tx, &stop),
        publish(
            Inputs {
                claude,
                codex,
                swap,
                lb,
                activity
            },
            output,
            &stop
        ),
    );
    result
}
fn ticker(period: Duration) -> tokio::time::Interval {
    let mut ticker = interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker
}
async fn tick(ticker: &mut tokio::time::Interval, stop: &CancellationToken) -> bool {
    tokio::select! { biased; _ = stop.cancelled() => false, _ = ticker.tick() => true }
}
// Watch versions coalesce a burst and remain pending during in-flight I/O. A
// one-second floor prevents repeated setup polling from creating a busy loop.
async fn source_tick(
    timer: &mut tokio::time::Interval,
    refresh: &mut watch::Receiver<u64>,
    stop: &CancellationToken,
) -> Option<bool> {
    tokio::select! {
        biased;
        _ = stop.cancelled() => None,
        changed = refresh.changed() => {
            if changed.is_err() { return None; }
            tokio::select! { biased; _ = stop.cancelled() => return None, _ = tokio::time::sleep(PUBLISH) => {} }
            refresh.borrow_and_update();
            timer.reset();
            Some(true)
        },
        _ = timer.tick() => Some(false),
    }
}
async fn oauth(
    mut owner: usage_oauth::Owner,
    output: watch::Sender<Snapshot>,
    mut refresh: watch::Receiver<u64>,
    stop: &CancellationToken,
) {
    let mut timer = ticker(REFRESH);
    while let Some(force) = source_tick(&mut timer, &mut refresh, stop).await {
        if force {
            owner.invalidate();
        }
        output.send_replace(owner.refresh(Utc::now(), stop).await);
    }
}
async fn swap_source(
    owner: Option<usage_sources::Cswap>,
    output: watch::Sender<Option<cswap::Parsed>>,
    mut refresh: watch::Receiver<u64>,
    stop: &CancellationToken,
) {
    let Some(mut owner) = owner else { return };
    let mut timer = ticker(REFRESH);
    while let Some(force) = source_tick(&mut timer, &mut refresh, stop).await {
        if force {
            owner.invalidate();
        }
        let now = Utc::now();
        let _ = owner.refresh(now, stop).await;
        output.send_replace(owner.current(Utc::now()));
    }
}
async fn lb_source(
    options: &usage_config::Options,
    client: usage_http::Client,
    mut accounts: Option<usage_sources::Accounts>,
    output: watch::Sender<Lb>,
    mut refresh: watch::Receiver<u64>,
    stop: &CancellationToken,
) {
    let loaded = options.load_lb(stop).await;
    let mut owner =
        loaded.and_then(|(endpoint, key)| usage_lb::Owner::new(endpoint, key, client.clone()).ok());
    let mut selected = owner.is_some();
    let mut timer = ticker(REFRESH);
    let mut seq: i64 = 0;
    while let Some(force) = source_tick(&mut timer, &mut refresh, stop).await {
        if force {
            // Drop the two-second read cache and last account projection.
            accounts = options
                .accounts
                .clone()
                .and_then(|path| usage_sources::Accounts::new(path).ok());
            // Reload private key files after account/setup changes; a failed new
            // credential cannot keep a previous account's quota alive.
            owner = options.load_lb(stop).await.and_then(|(endpoint, key)| {
                usage_lb::Owner::new(endpoint, key, client.clone()).ok()
            });
            selected = owner.is_some();
        }
        seq = seq.saturating_add(1);
        let now = Utc::now();
        let mut snapshot = match &mut owner {
            Some(owner) => owner.refresh(now, stop).await,
            None => usage_lb::unavailable(seq, now, "networkError"),
        };
        if let Some(accounts) = &accounts {
            if let Ok(export) = accounts.read(Utc::now(), stop).await {
                snapshot.accounts = export.accounts;
                snapshot.accounts_updated_at = export.updated_at;
            }
        }
        output.send_replace(Lb { selected, snapshot });
    }
}
async fn activity_source(
    owner: Option<usage_activity::Reader>,
    output: watch::Sender<Option<usage_activity::Sample>>,
    stop: &CancellationToken,
) {
    let Some(owner) = owner else { return };
    let mut timer = ticker(SCAN);
    while tick(&mut timer, stop).await {
        // Do not keep yesterday's counters or a live burn rate through repeated
        // timeouts. The reader retains its own offsets/trackers for recovery.
        output.send_replace(owner.sample(Utc::now(), stop).await.ok());
    }
}
fn activity(snapshot: &mut Snapshot, activity: &ActivitySnapshot) {
    snapshot.burn_rate_per_min = activity.rate_per_minute;
    snapshot.burn_state = activity.state.as_str().into();
    snapshot.today_total_tokens = activity.today_total_tokens;
    snapshot.today_sessions = activity.today_sessions_count as i64;
    if activity.has_observed && snapshot.status.data_source == "api_only" {
        snapshot.status.data_source = "api+jsonl".into();
    }
}
impl Inputs {
    fn dirty(&self) -> bool {
        self.claude.has_changed().unwrap_or(false)
            || self.codex.has_changed().unwrap_or(false)
            || self.swap.has_changed().unwrap_or(false)
            || self.lb.has_changed().unwrap_or(false)
            || self.activity.has_changed().unwrap_or(false)
    }
    fn assemble(&mut self, seq: i64, now: DateTime<Utc>) -> Result<Latest, Error> {
        let mut claude = self.claude.borrow_and_update().clone();
        let mut codex = self.codex.borrow_and_update().clone();
        // Read activity only after source results. No source worker carries a
        // stale copy of activity back into this publication owner.
        if let Some(current) = self.activity.borrow_and_update().as_ref() {
            activity(&mut claude, &current.claude);
            activity(&mut codex, &current.codex);
        }
        for snapshot in [&mut claude, &mut codex] {
            snapshot.seq = seq;
            snapshot.generated_at_utc = format_time(now);
        }
        let parsed = self
            .swap
            .borrow_and_update()
            .clone()
            .unwrap_or(cswap::Parsed {
                accounts: Vec::new(),
                updated_at: None,
            });
        let swap = sources::claude_swap(&claude, parsed, now).map_err(|_| Error::Encoding)?;
        let lb = self.lb.borrow_and_update();
        let claude = sources::bundle(&claude, &swap, false).map_err(|_| Error::Encoding)?;
        let mut codex =
            sources::bundle(&codex, &lb.snapshot, lb.selected).map_err(|_| Error::Encoding)?;
        codex.seq = seq;
        codex.generated_at_utc = format_time(now);
        transport::validate(&claude).map_err(|_| Error::Encoding)?;
        transport::validate(&codex).map_err(|_| Error::Encoding)?;
        Ok(Latest {
            claude: Arc::new(claude),
            codex: Arc::new(codex),
        })
    }
}
async fn publish(
    mut inputs: Inputs,
    output: watch::Sender<Option<Latest>>,
    stop: &CancellationToken,
) -> Result<(), Error> {
    let mut timer = ticker(PUBLISH);
    let mut seq: i64 = 0;
    let mut since_publish = HEARTBEAT_TICKS;
    while tick(&mut timer, stop).await {
        since_publish = since_publish.saturating_add(1);
        if inputs.dirty() || since_publish >= HEARTBEAT_TICKS {
            seq = seq.saturating_add(1);
            match inputs.assemble(seq, Utc::now()) {
                Ok(latest) => {
                    output.send_replace(Some(latest));
                    since_publish = 0;
                }
                Err(error) => {
                    output.send_replace(None);
                    stop.cancel();
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

/// Link-local forwarding only. A slow/disconnected link retains one latest pair,
/// while the shared collector continues with no per-link source/task growth.
pub(crate) async fn forward(
    mut latest: Receiver,
    sender: Sender,
    protocol: Negotiated,
    stop: CancellationToken,
) -> Result<(), peer::Error> {
    loop {
        let current = latest.borrow_and_update().clone();
        match current {
            Some(current) => {
                for snapshot in [current.claude, current.codex] {
                    let snapshot = snapshots::usage_to_proto((*snapshot).clone())
                        .map_err(|_| peer::Error::Encoding)?;
                    peer::send(
                        &sender,
                        protocol,
                        p::envelope::Body::Usage(Box::new(snapshot)),
                        stop.clone(),
                    )
                    .await?;
                }
            }
            None => {
                peer::send(
                    &sender,
                    protocol,
                    p::envelope::Body::UsageUnavailable(p::Empty {}),
                    stop.clone(),
                )
                .await?
            }
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            result = latest.changed() => if result.is_err() {
                // A stopped source must not leave apparently live usage behind.
                peer::send(&sender, protocol, p::envelope::Body::UsageUnavailable(p::Empty {}), stop.clone()).await?;
                return Ok(());
            },
        }
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
