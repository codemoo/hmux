//! Home shared tabs. One bounded store survives reconnects; catalog/recovery
//! reads complete before acquiring the Go-compatible workspace transaction lock.
use crate::catalog::TmuxCatalogReader;
use crate::observation;
use bytes::Bytes;
use hmux_core::{command::CommandRunner, workspace::Store, PrivateDir};
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
        let started = std::time::Instant::now();
        let fetch_stop = stop.clone();
        let recovery = self.recovery.clone();
        let allowed_stop = stop.clone();
        let work = self.store.sync(
            None,
            change,
            move || {
                let mut catalog = match runtime.block_on(reader.read_basic_cancelable(
                    &runner,
                    &fetch_stop,
                    deadline,
                )) {
                    Ok(catalog) => catalog,
                    Err(error) => {
                        if let Some(report) = &reporter {
                            report(observation::Event::new(
                                observation::Stage::Action,
                                Some(hmux_protocol::protobuf::types::Operation::Workspace),
                                observation::Reason::catalog(&error),
                                started,
                            ));
                        }
                        return Err(Error::Unavailable);
                    }
                };
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
