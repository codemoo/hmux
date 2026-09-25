//! Exact-lifetime public transcript reads. File paths and provider record IDs
//! remain private; read/selection failures return only a public status category.
use crate::{
    binding::{Binding, Status},
    catalog::{CatalogError, TmuxCatalogReader},
    inspection::{self, Error, Inspector, ScanPurpose},
    records, transcript,
};
use bytes::Bytes;
use hmux_model::{
    Conversation, SessionIdentity, CONVERSATION_AMBIGUOUS, CONVERSATION_READY,
    CONVERSATION_UNAVAILABLE,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
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
}
impl Job {
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
    fn binding(
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
    fn read(
        &self,
        runtime: &tokio::runtime::Handle,
        deadline: Instant,
    ) -> Result<Conversation, Error> {
        let attempt: Result<Conversation, Error> = (|| {
            let Some(first) = self.pane(runtime, deadline)? else {
                return Ok(self.empty(Status::Unavailable));
            };
            let (binding, status) = self.binding(first, runtime, deadline)?;
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
            let (second, status) = self.binding(first, runtime, deadline)?;
            if status != Status::Ready {
                return Ok(self.empty(status));
            }
            let second = second.ok_or(Error::Unavailable)?;
            if !binding.same_record(&second) {
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
                status: CONVERSATION_READY.into(),
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
