//! Exact-lifetime public transcript reads. File paths and provider record IDs
//! remain private; read/selection failures return only a public status category.
use crate::{
    binding::{Binding, Status},
    catalog::{CatalogError, TmuxCatalogReader},
    conversation_link::{self, Link},
    inspection::{self, Error, Inspector, ScanPurpose},
    observation, records, status_probe, transcript,
};
use bytes::Bytes;
use hmux_model::{
    Conversation, SessionIdentity, CONVERSATION_AMBIGUOUS, CONVERSATION_LINKED, CONVERSATION_READY,
    CONVERSATION_UNAVAILABLE,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;
const ENCODED_LIMIT: usize = 2 * 1024 * 1024 - 4096;
const TEXT_LIMIT: usize = 512 * 1024;

pub(crate) struct Job {
    pub inspector: Arc<Inspector>,
    pub reader: TmuxCatalogReader,
    pub identity: SessionIdentity,
    pub stop: CancellationToken,
    pub state_dir: PathBuf,
    pub reporter: Option<observation::Reporter>,
}
impl Job {
    pub async fn link(
        self,
        record: Option<(String, PathBuf, bool)>,
        permit: OwnedSemaphorePermit,
    ) -> Result<(), Error> {
        let runtime = tokio::runtime::Handle::current();
        let _cancel = self.stop.clone().drop_guard();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let deadline = Instant::now() + Duration::from_secs(15);
            if record.is_none() {
                inspection::check(&self.stop, deadline)?;
                return conversation_link::save(&self.state_dir, &self.identity, None);
            }
            let pane = self.pane(&runtime, deadline)?.ok_or(Error::Unavailable)?;
            let linked = if let Some((thread, path, notify)) = record {
                let (base, status) = self.automatic_binding(pane, &runtime, deadline)?;
                if status != Status::Unavailable {
                    return Err(Error::Unavailable);
                }
                let base = base.ok_or(Error::Unavailable)?;
                let stamp = self.inspector.process_stamp(
                    base.provider_pid,
                    &self.stop,
                    deadline,
                    &runtime,
                )?;
                let mut link = Link::new(
                    self.identity.clone(),
                    pane,
                    &base,
                    stamp.clone(),
                    (thread, path),
                    &self.stop,
                    deadline,
                )?;
                if notify {
                    link.enable_notifications()?;
                }
                let (after, status) = self.automatic_binding(pane, &runtime, deadline)?;
                let after = after.ok_or(Error::Unavailable)?;
                if status != Status::Unavailable
                    || !link.matches(&self.identity, pane, &after)
                    || self.inspector.process_stamp(
                        after.provider_pid,
                        &self.stop,
                        deadline,
                        &runtime,
                    )? != stamp
                {
                    return Err(Error::Unavailable);
                }
                Some(link)
            } else {
                None
            };
            if self.pane(&runtime, deadline)? != Some(pane) {
                return Err(Error::Unavailable);
            }
            inspection::check(&self.stop, deadline)?;
            conversation_link::save(&self.state_dir, &self.identity, linked.as_ref())
        })
        .await
        .map_err(|_| Error::Worker)?
    }
    pub async fn run(mut self, permit: OwnedSemaphorePermit) -> Result<Conversation, Error> {
        if hmux_model::validate_session_id(&self.identity.id).is_err()
            || self.identity.created_at < 1
        {
            return Err(Error::Invalid);
        }
        let child = self.stop.child_token();
        let _cancel = child.clone().drop_guard();
        self.stop = child;
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let deadline = Instant::now() + Duration::from_secs(15);
            let value = self.read(&runtime, deadline)?;
            encode(value)
        })
        .await
        .map_err(|_| Error::Worker)?
    }
    fn empty(&self, status: Status) -> Conversation {
        Conversation {
            session_id: self.identity.id.clone(),
            created_at: self.identity.created_at,
            status: match status {
                Status::Ambiguous => CONVERSATION_AMBIGUOUS,
                _ => CONVERSATION_UNAVAILABLE,
            }
            .into(),
            messages: Some(Vec::new()),
            ..Conversation::default()
        }
    }
    fn pane(
        &self,
        runtime: &tokio::runtime::Handle,
        deadline: Instant,
    ) -> Result<Option<i32>, Error> {
        inspection::check(&self.stop, deadline)?;
        let value = runtime
            .block_on(self.reader.read_basic_cancelable(
                inspection::commands(),
                &self.stop,
                deadline.into(),
            ))
            .map_err(|error| match error {
                CatalogError::Command(error)
                    if error.kind() == hmux_core::command::RunErrorKind::Busy =>
                {
                    Error::Busy
                }
                _ => Error::Unavailable,
            })?;
        Ok(value
            .sessions
            .unwrap_or_default()
            .iter()
            .find(|s| s.id == self.identity.id && s.created_at == self.identity.created_at)
            .and_then(|s| i32::try_from(s.pane_pid).ok().filter(|&p| p > 0)))
    }
    fn automatic_binding(
        &self,
        pane: i32,
        runtime: &tokio::runtime::Handle,
        deadline: Instant,
    ) -> Result<(Option<Arc<Binding>>, Status), Error> {
        // Public text needs exact ownership, not another model/activity-tail scan.
        let mut scan = self.inspector.scan(
            &[pane],
            ScanPurpose::Conversation,
            &self.stop,
            deadline,
            runtime,
        )?;
        Ok((
            scan.bindings.remove(&pane),
            scan.statuses.remove(&pane).unwrap_or(Status::Unavailable),
        ))
    }
    fn binding(
        &self,
        pane: i32,
        runtime: &tokio::runtime::Handle,
        deadline: Instant,
    ) -> Result<(Option<Arc<Binding>>, Status, bool), Error> {
        let (base, status) = self.automatic_binding(pane, runtime, deadline)?;
        if status == Status::Unavailable {
            if let (Some(base), Some(link)) = (
                base.as_ref(),
                conversation_link::load(&self.state_dir, &self.identity)?,
            ) {
                if link.matches(&self.identity, pane, base) {
                    let stamp = self.inspector.process_stamp(
                        base.provider_pid,
                        &self.stop,
                        deadline,
                        runtime,
                    )?;
                    let bound = link.resolve(&stamp, &self.stop, deadline)?;
                    return Ok((Some(Arc::new(bound)), Status::Ready, true));
                }
            }
        }
        Ok((base, status, false))
    }
    fn read(
        &self,
        runtime: &tokio::runtime::Handle,
        deadline: Instant,
    ) -> Result<Conversation, Error> {
        let attempt: Result<Conversation, Error> = (|| {
            let Some(first) = self.pane(runtime, deadline)? else {
                return Ok(self.empty(Status::Unavailable));
            };
            let (mut binding, mut status, mut linked) = match self.binding(first, runtime, deadline)
            {
                Ok(value) => value,
                Err(error @ (Error::Busy | Error::Cancelled)) => return Err(error),
                Err(_) => (None, Status::Unavailable, false),
            };
            if status == Status::Unavailable {
                let (base, automatic) = self.automatic_binding(first, runtime, deadline)?;
                if automatic == Status::Unavailable {
                    if let Some(base) = base {
                        let probe = status_probe::Probe {
                            reporter: self.reporter.as_ref(),
                            identity: &self.identity,
                            pane: first,
                            base: &base,
                            reader: &self.reader,
                            inspector: &self.inspector,
                            state_dir: &self.state_dir,
                            stop: &self.stop,
                            deadline,
                            runtime,
                        };
                        if probe.run().is_ok() {
                            (binding, status, linked) = self.binding(first, runtime, deadline)?;
                        }
                    }
                }
            }
            if status != Status::Ready {
                return Ok(self.empty(status));
            }
            let binding = binding.ok_or(Error::Unavailable)?;
            let mut file = records::open_record(&binding.root, &binding.path)
                .map_err(|_| Error::Unavailable)?;
            let before = file.metadata().map_err(|_| Error::Unavailable)?;
            let (tail, offset) = read_tail(&mut file, before.len(), &self.stop, deadline)?;
            drop(file);
            let mut identity = [0u8; 16];
            identity[..8].copy_from_slice(&before.dev().to_be_bytes());
            identity[8..].copy_from_slice(&before.ino().to_be_bytes());
            let parsed =
                transcript::parse_tail(&tail, offset, identity, binding.provider, &self.stop)
                    .map_err(|_| Error::Unavailable)?;
            drop(tail);
            inspection::check(&self.stop, deadline)?;
            if self.pane(runtime, deadline)? != Some(first) {
                return Ok(self.empty(Status::Unavailable));
            }
            let (second, status, linked_after) = self.binding(first, runtime, deadline)?;
            if status != Status::Ready {
                return Ok(self.empty(status));
            }
            let second = second.ok_or(Error::Unavailable)?;
            if linked != linked_after || !binding.same_record(&second) {
                return Ok(self.empty(Status::Ambiguous));
            }
            let file =
                records::open_record(&second.root, &second.path).map_err(|_| Error::Unavailable)?;
            let after = file.metadata().map_err(|_| Error::Unavailable)?;
            if before.dev() != after.dev()
                || before.ino() != after.ino()
                || before.len() > after.len()
            {
                return Ok(self.empty(Status::Ambiguous));
            }
            Ok(Conversation {
                session_id: self.identity.id.clone(),
                created_at: self.identity.created_at,
                provider: binding.provider.as_str().into(),
                status: if linked {
                    CONVERSATION_LINKED
                } else {
                    CONVERSATION_READY
                }
                .into(),
                messages: Some(parsed.messages),
                truncated: parsed.truncated,
            })
        })();
        inspection::check(&self.stop, deadline)?;
        match attempt {
            Ok(value) => Ok(value),
            Err(Error::Busy) => Err(Error::Busy),
            Err(_) => Ok(self.empty(Status::Unavailable)),
        }
    }
}
fn read_tail(
    file: &mut File,
    size: u64,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<(Vec<u8>, u64), Error> {
    let start = size.saturating_sub(transcript::TAIL_LIMIT as u64);
    let count = (size - start) as usize;
    file.seek(SeekFrom::Start(start))
        .map_err(|_| Error::Unavailable)?;
    let mut data = vec![0; count];
    for chunk in data.chunks_mut(64 * 1024) {
        inspection::check(stop, deadline)?;
        file.read_exact(chunk).map_err(|_| Error::Unavailable)?;
    }
    Ok((data, start))
}
struct Counter(usize);
impl Write for Counter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(data.len())
            .ok_or_else(|| std::io::Error::other("size overflow"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(mut value: Conversation) -> Result<Conversation, Error> {
    let messages = value.messages.as_mut().ok_or(Error::Invalid)?;
    let mut total = messages.iter().map(|m| m.text.len()).sum::<usize>();
    let mut drop_count = 0;
    while total > TEXT_LIMIT {
        total -= messages[drop_count].text.len();
        drop_count += 1;
    }
    if drop_count > 0 {
        messages.drain(..drop_count);
        value.truncated = true;
    }
    loop {
        let mut count = Counter(0);
        serde_json::to_writer(&mut count, &value).map_err(|_| Error::Unavailable)?;
        if count.0 <= ENCODED_LIMIT {
            return Ok(value);
        }
        let messages = value.messages.as_mut().expect("checked");
        if messages.is_empty() {
            return Err(Error::Unavailable);
        }
        messages.remove(0);
        value.truncated = true;
    }
}
pub(crate) fn json(value: &Conversation) -> Result<Bytes, Error> {
    let raw = serde_json::to_vec(value).map_err(|_| Error::Unavailable)?;
    if raw.len() > ENCODED_LIMIT {
        return Err(Error::Unavailable);
    }
    Ok(Bytes::from(raw))
}
