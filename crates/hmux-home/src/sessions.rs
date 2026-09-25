//! Session creation and reversible metadata actions. One blocking owner retains
//! admission across filesystem work, bounded tmux commands and metadata commit.
//! Failure or cancellation never kills an original or newly created session.
use crate::{catalog::TmuxCatalogReader, config, create_plan::Plan, sessionstate, view};
use hmux_core::command::CommandRunner;
use hmux_model::{Catalog, Session, SessionIdentity};
use hmux_protocol::protobuf::types as p;
use std::{
    ffi::OsString,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

static ACTION_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static CATALOG_SLOT: OnceLock<Arc<Semaphore>> = OnceLock::new();
static COMMANDS: OnceLock<CommandRunner> = OnceLock::new();
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Cancelled,
    CreatedUnrecorded,
    Worker,
}

/// Snapshot host inputs once. No environment changes or private state I/O occur
/// here; all actual actions use the explicitly selected inventory and tmux target.
#[derive(Clone)]
pub struct Context {
    home: PathBuf,
    path: OsString,
    shell: OsString,
}
impl Context {
    pub fn new(home: PathBuf, path: OsString, shell: OsString) -> Result<Self, Error> {
        if !home.is_absolute()
            || home.as_os_str().as_bytes().len() > 4096
            || path.as_bytes().len() > 64 * 1024
            || shell.as_bytes().len() > 4096
            || [home.as_os_str(), &path, &shell]
                .iter()
                .any(|v| v.as_bytes().contains(&0))
        {
            return Err(Error::Invalid);
        }
        Ok(Self { home, path, shell })
    }
}
pub(crate) fn admit() -> Option<OwnedSemaphorePermit> {
    ACTION_SLOT
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
        .ok()
}
pub(crate) fn supported(operation: i32) -> bool {
    matches!(
        p::Operation::try_from(operation),
        Ok(p::Operation::Create | p::Operation::Alias | p::Operation::Hidden)
    )
}

fn created(raw: &[u8]) -> Result<p::CreatedResult, Error> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::CreatedUnrecorded)?;
    let mut fields = text.split_whitespace();
    let id = fields.next().ok_or(Error::CreatedUnrecorded)?;
    let created_at = fields
        .next()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&v| v > 0)
        .ok_or(Error::CreatedUnrecorded)?;
    if fields.next().is_some() || hmux_model::validate_session_id(id).is_err() {
        return Err(Error::CreatedUnrecorded);
    }
    Ok(p::CreatedResult {
        id: id.into(),
        created_at,
        reused: false,
    })
}
fn check(stop: &CancellationToken, deadline: std::time::Instant) -> Result<(), Error> {
    if stop.is_cancelled() || std::time::Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) struct Job {
    pub context: Arc<Context>,
    pub config: config::HomeConfig,
    pub target: view::Target,
    pub catalog: TmuxCatalogReader,
    pub request: p::Request,
    pub stop: CancellationToken,
}
impl Job {
    pub async fn run(mut self, permit: OwnedSemaphorePermit) -> Result<p::response::Result, Error> {
        let stop = self.stop.child_token();
        let _cancel_on_drop = stop.clone().drop_guard();
        self.stop = stop;
        let runtime = tokio::runtime::Handle::current();
        // A caller abort cancels work, but the blocking task and its permit live
        // until the OS work and direct child cleanup have actually completed.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            self.work(runtime)
        })
        .await
        .map_err(|_| Error::Worker)?
    }

    fn work(self, runtime: tokio::runtime::Handle) -> Result<p::response::Result, Error> {
        let deadline = std::time::Instant::now() + TIMEOUT;
        check(&self.stop, deadline)?;
        let runner =
            COMMANDS.get_or_init(|| CommandRunner::new(1).expect("nonzero session command limit"));
        let store = sessionstate::Store::new(self.config.state_dir);
        match p::Operation::try_from(self.request.operation).map_err(|_| Error::Invalid)? {
            p::Operation::Create => {
                let Some(p::request::Payload::Create(q)) = self.request.payload else {
                    return Err(Error::Invalid);
                };
                let inventory = config::load_inventory(&self.config.inventory_path)
                    .map_err(|_| Error::Unavailable)?;
                let plan = Plan::prepare(
                    &inventory,
                    q.profile.as_deref().unwrap_or_default(),
                    q.name.as_deref().unwrap_or_default(),
                    &self.context.home,
                    &self.context.path,
                    &self.context.shell,
                )
                .map_err(|_| Error::Invalid)?;
                let alias = q.name.as_deref().unwrap_or_default().trim();
                // Validate display metadata before allocating or starting tmux.
                sessionstate::validate_alias(alias).map_err(|_| Error::Invalid)?;
                check(&self.stop, deadline)?;
                let allocated = plan.allocate().map_err(|_| Error::Unavailable)?;
                check(&self.stop, deadline)?;
                let mut args: Vec<OsString> = [
                    "new-session",
                    "-d",
                    "-P",
                    "-F",
                    "#{session_id} #{session_created}",
                    "-s",
                ]
                .into_iter()
                .map(Into::into)
                .collect();
                args.push(allocated.name.clone().into());
                args.push("-c".into());
                args.push(allocated.directory.into_os_string());
                args.extend(allocated.command);
                let raw = runtime
                    .block_on(
                        self.target
                            .command_os(runner, args, &self.stop, deadline.into()),
                    )
                    .map_err(|_| Error::Unavailable)?;
                let result = created(&raw)?;
                let session = Session {
                    id: result.id.clone(),
                    name: allocated.name,
                    created_at: result.created_at,
                    alias: alias.to_owned(),
                    ..Session::default()
                };
                store
                    .set_profile(&session, &allocated.profile, self.stop.clone(), deadline)
                    .map_err(|_| Error::CreatedUnrecorded)?;
                Ok(p::response::Result::Created(result))
            }
            p::Operation::Alias | p::Operation::Hidden => {
                let session = self.request.session.ok_or(Error::Invalid)?;
                let identity = SessionIdentity {
                    id: session.id,
                    created_at: session.created_at,
                };
                let resolve = || {
                    let catalog = runtime
                        .block_on(
                            self.catalog.read_basic_cancelable(
                                runner,
                                &self.stop,
                                deadline
                                    .min(std::time::Instant::now() + Duration::from_secs(5))
                                    .into(),
                            ),
                        )
                        .map_err(|_| sessionstate::Error::Changed)?;
                    catalog
                        .sessions
                        .unwrap_or_default()
                        .into_iter()
                        .find(|s| s.id == identity.id)
                        .ok_or(sessionstate::Error::Changed)
                };
                if self.request.operation == p::Operation::Alias as i32 {
                    let Some(p::request::Payload::Alias(q)) = self.request.payload else {
                        return Err(Error::Invalid);
                    };
                    store
                        .set_alias_expected(
                            &identity,
                            q.alias.as_deref().unwrap_or_default(),
                            self.stop.clone(),
                            deadline,
                            resolve,
                        )
                        .map_err(|_| Error::Unavailable)?;
                } else {
                    let Some(p::request::Payload::Hidden(q)) = self.request.payload else {
                        return Err(Error::Invalid);
                    };
                    store
                        .set_hidden_expected(
                            &identity,
                            q.hidden,
                            self.stop.clone(),
                            deadline,
                            resolve,
                        )
                        .map_err(|_| Error::Unavailable)?;
                }
                Ok(p::response::Result::Ok(p::Empty {}))
            }
            _ => Err(Error::Invalid),
        }
    }
}

/// A separate single blocking slot prevents catalog reads from accumulating and
/// keeps metadata actions independent of catalog polling. No stale state cache.
pub(crate) async fn overlay(
    mut catalog: Catalog,
    state_dir: PathBuf,
    recovery: Option<crate::recovery::Store>,
    stop: &CancellationToken,
) -> Result<Catalog, Error> {
    let permit = tokio::select! {
        biased;
        _ = stop.cancelled() => return Err(Error::Cancelled),
        permit = CATALOG_SLOT.get_or_init(|| Arc::new(Semaphore::new(1))).clone().acquire_owned() => permit.map_err(|_| Error::Unavailable)?,
    };
    let cancel = stop.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let workflow = crate::workflow::Store::new(state_dir.clone());
        let store = sessionstate::Store::new(state_dir);
        store.apply(&mut catalog).map_err(|_| Error::Unavailable)?;
        store
            .apply_visibility(&mut catalog)
            .map_err(|_| Error::Unavailable)?;
        if let Some(recovery) = recovery {
            let _ = recovery.apply(&mut catalog);
        }
        // Optional observability must never take the terminal/catalog offline.
        let _ = workflow.apply(&mut catalog, chrono::Utc::now(), &cancel);
        Ok(catalog)
    })
    .await
    .map_err(|_| Error::Worker)?
}
