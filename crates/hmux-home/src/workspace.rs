//! Home shared tabs. One bounded store survives reconnects; catalog/recovery
//! reads complete before acquiring the Go-compatible workspace transaction lock.
use crate::catalog::{CatalogError, TmuxCatalogReader};
use crate::observation;
use bytes::Bytes;
use hmux_core::{
    command::{CommandRunner, RunErrorKind},
    workspace::Store,
    PrivateDir,
};
use hmux_model::workspace::{Change, SessionLineage, Snapshot};
use serde::Deserialize;
use std::{path::Path, time::Duration};
use tokio_util::sync::CancellationToken;

pub use hmux_core::workspace::Error;
#[derive(Clone)]
pub struct Workspace {
    store: Store,
    recovery: Option<crate::recovery::Store>,
}
impl Workspace {
    /// Startup only, after the connector has acquired its singleton.
    pub fn open(state_dir: &Path) -> Result<Self, Error> {
        Ok(Self {
            recovery: None,
            store: Store::new(
                PrivateDir::open_or_create_trusted(state_dir).map_err(|_| Error::Unavailable)?,
            ),
        })
    }
    pub fn with_recovery(mut self, recovery: crate::recovery::Store) -> Self {
        self.recovery = Some(recovery);
        self
    }
    pub async fn request(
        &self,
        raw: &[u8],
        reader: TmuxCatalogReader,
        runner: CommandRunner,
        cancel: &CancellationToken,
    ) -> Result<Bytes, Error> {
        let change = decode(raw)?;
        let snapshot = self.request_typed(change, reader, runner, cancel).await?;
        encode(snapshot)
    }
    pub async fn request_typed(
        &self,
        change: Option<Change>,
        reader: TmuxCatalogReader,
        runner: CommandRunner,
        cancel: &CancellationToken,
    ) -> Result<Snapshot, Error> {
        self.request_typed_reported(change, reader, runner, cancel, None)
            .await
    }
    pub(crate) async fn request_typed_reported(
        &self,
        change: Option<Change>,
        reader: TmuxCatalogReader,
        runner: CommandRunner,
        cancel: &CancellationToken,
        reporter: Option<observation::Reporter>,
    ) -> Result<Snapshot, Error> {
        if change.as_ref().is_some_and(|v| !v.valid()) {
            return Err(Error::Invalid);
        }
        let stop = cancel.child_token();
        let _cancel_on_drop = stop.clone().drop_guard();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let runtime = tokio::runtime::Handle::current();
        let fetch_stop = stop.clone();
        let recovery = self.recovery.clone();
        let allowed_stop = stop.clone();
        let work = self.store.sync(
            None,
            change,
            move || {
                let catalog_started = std::time::Instant::now();
                let mut catalog = match runtime.block_on(reader.read_basic_cancelable(
                    &runner,
                    &fetch_stop,
                    deadline,
                )) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        if let Some(report) = &reporter {
                            report(observation::Event::new(
                                observation::Stage::WorkspaceCatalog,
                                Some(hmux_protocol::protobuf::types::Operation::Workspace),
                                observation::Reason::catalog(&error),
                                catalog_started,
                            ));
                        }
                        return Err(match error {
                            CatalogError::Command(command)
                                if command.kind() == RunErrorKind::Busy =>
                            {
                                Error::Busy
                            }
                            _ => Error::Unavailable,
                        });
                    }
                };
                if catalog_started.elapsed() >= Duration::from_millis(500) {
                    if let Some(report) = &reporter {
                        report(observation::Event::new(
                            observation::Stage::WorkspaceCatalog,
                            Some(hmux_protocol::protobuf::types::Operation::Workspace),
                            observation::Reason::Slow,
                            catalog_started,
                        ));
                    }
                }
                if let Some(recovery) = recovery {
                    let _ = recovery.apply(&mut catalog);
                }
                Ok(catalog
                    .sessions
                    .iter()
                    .flatten()
                    .map(SessionLineage::from)
                    .collect())
            },
            move || !allowed_stop.is_cancelled() && tokio::time::Instant::now() < deadline,
        );
        let snapshot = tokio::select! {
            biased;
            _ = stop.cancelled() => return Err(Error::Cancelled),
            result = tokio::time::timeout_at(deadline, work) => result.map_err(|_| Error::Cancelled)??,
        };
        Ok(snapshot)
    }
    pub async fn shutdown(&self) {
        self.store.shutdown().await;
    }
}
fn decode(raw: &[u8]) -> Result<Option<Change>, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Request {
        #[serde(default)]
        change: Option<Change>,
    }
    // The agent CLI accepts a 32 KiB Go Change and wraps it in {"change":...}.
    if raw.len() > hmux_model::workspace::MAX_BYTES + 32
        || raw.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
    {
        return Err(Error::Invalid);
    }
    let request: Request = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    if request.change.as_ref().is_some_and(|v| !v.valid()) {
        return Err(Error::Invalid);
    }
    Ok(request.change)
}
fn encode(value: Snapshot) -> Result<Bytes, Error> {
    let bytes = serde_json::to_vec(&value).map_err(|_| Error::Invalid)?;
    if bytes.len() > hmux_model::workspace::MAX_BYTES {
        return Err(Error::Invalid);
    }
    Ok(bytes.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmux_core::command::CommandSpec;
    use std::{
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[tokio::test]
    async fn busy_catalog_command_stays_busy_through_workspace() {
        let dir = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-workspace-busy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let runner = CommandRunner::new(1).unwrap();
        let holder = {
            let runner = runner.clone();
            tokio::spawn(async move {
                runner
                    .run(CommandSpec::new("/bin/sleep", 1024, Duration::from_secs(2)).arg("1"))
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            while runner.available_slots() != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let workspace = Workspace::open(&dir).unwrap();
        let reader =
            TmuxCatalogReader::new("/bin/true".into(), None, Duration::from_secs(1)).unwrap();
        let result = workspace
            .request_typed(None, reader, runner, &CancellationToken::new())
            .await;
        assert!(matches!(result, Err(Error::Busy)));
        holder.await.unwrap().unwrap();
        workspace.shutdown().await;
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn request_is_bounded_and_validates_changes_before_io() {
        assert!(decode(b"{}").unwrap().is_none());
        assert!(decode(b"{\"change\":null}").unwrap().is_none());
        for raw in [
            b"null".as_slice(),
            b"[]",
            b"{} {}",
            b"{\"extra\":0}",
            b"{\"change\":{}}",
            &vec![b' '; (16 << 10) + 1],
        ] {
            assert!(decode(raw).is_err());
        }
    }
}
