//! Bounded Codex task-completion observation for an admitted blocking worker.
//! The caller supplies freshly resolved bindings; this module never discovers
//! processes or retains transcript contents.
use crate::binding::{Binding, Provider, Status};
use crate::records::open_record;
use chrono::{DateTime, Utc};
use hmux_model::SessionIdentity;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

const TAIL_MAX: u64 = 8 * 1024 * 1024;
const LINE_MAX: usize = 2 * 1024 * 1024;
const CHUNK: usize = 32 * 1024;
const CURSORS_MAX: usize = 4096;
const RETAINED_MAX: usize = 4 * 1024 * 1024;
const EMITTED_MAX: usize = 64;
const ANCHOR_MAX: u64 = 256;

#[derive(Clone)]
pub struct Observation {
    pub identity: SessionIdentity,
    pub binding: Option<Arc<Binding>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub identity: SessionIdentity,
    pub id: String,
    pub completed_at: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Limit,
    Unavailable,
}

#[derive(Default)]
pub struct Tracker {
    cursors: HashMap<SessionIdentity, Cursor>,
}

struct Cursor {
    root: PathBuf,
    path: PathBuf,
    record_id: String,
    dev: u64,
    ino: u64,
    offset: u64,
    anchor: [u8; 32],
    anchor_len: u64,
    armed: bool,
}

#[derive(Deserialize)]
struct Event<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    payload: Option<Payload<'a>>,
}

#[derive(Deserialize)]
struct Payload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<Cow<'a, str>>,
}

fn check(stop: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if stop.is_cancelled() || Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn anchor(
    file: &mut File,
    offset: u64,
    stop: &CancellationToken,
    deadline: Instant,
) -> Result<([u8; 32], u64), Error> {
    let len = offset.min(ANCHOR_MAX);
    file.seek(SeekFrom::Start(offset - len))
        .map_err(|_| Error::Unavailable)?;
    let mut bytes = [0u8; ANCHOR_MAX as usize];
    check(stop, deadline)?;
    file.read_exact(&mut bytes[..len as usize])
        .map_err(|_| Error::Unavailable)?;
    check(stop, deadline)?;
    Ok((Sha256::digest(&bytes[..len as usize]).into(), len))
}

// Only newline-terminated records advance the cursor. A cut tail skips its
// first fragment unless the byte before the cut is a newline.
fn scan(
    file: &mut File,
    start: u64,
    size: u64,
    tail: bool,
    stop: &CancellationToken,
    deadline: Instant,
    mut visit: impl FnMut(&[u8], u64),
) -> Result<u64, Error> {
    if size < start || size - start > TAIL_MAX {
        return Err(Error::Limit);
    }
    let mut skip = false;
    if tail && start > 0 {
        file.seek(SeekFrom::Start(start - 1))
            .map_err(|_| Error::Unavailable)?;
        let mut previous = [0u8; 1];
        file.read_exact(&mut previous)
            .map_err(|_| Error::Unavailable)?;
        skip = previous[0] != b'\n';
    }
    file.seek(SeekFrom::Start(start))
        .map_err(|_| Error::Unavailable)?;
    let mut line = Vec::new();
    let mut oversized = false;
    let mut at = start;
    let mut last_complete = start;
    let mut chunk = [0u8; CHUNK];
    while at < size {
        check(stop, deadline)?;
        let want = ((size - at) as usize).min(CHUNK);
        file.read_exact(&mut chunk[..want])
            .map_err(|_| Error::Unavailable)?;
        for byte in &chunk[..want] {
            if *byte == b'\n' {
                if !skip && !oversized && !line.is_empty() {
                    visit(&line, at - line.len() as u64);
                }
                line.clear();
                oversized = false;
                skip = false;
                last_complete = at + 1;
            } else if !skip && !oversized {
                if line.len() == LINE_MAX {
                    line.clear();
                    oversized = true;
                } else {
                    line.push(*byte);
                }
            }
            at += 1;
        }
    }
    check(stop, deadline)?;
    // Go baselines at EOF when a cut tail has no complete record. This
    // prevents an oversized suffix becoming a future apparent new event.
    if tail && start > 0 && last_complete == start && skip {
        Ok(size)
    } else {
        Ok(last_complete)
    }
}

fn event(line: &[u8]) -> Option<(&str, Option<String>)> {
    let value: Event<'_> = serde_json::from_slice(line).ok()?;
    let selected = if matches!(
        value.kind.as_deref(),
        Some("task_started" | "task_complete")
    ) {
        value.kind.as_deref()
    } else {
        value
            .payload
            .as_ref()
            .and_then(|payload| payload.kind.as_deref())
    };
    let kind = match selected {
        Some("task_started") => "task_started",
        Some("task_complete") => "task_complete",
        _ => return None,
    };
    let timestamp = value.timestamp.as_deref().and_then(|raw| {
        let date = DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
        if date.timestamp() == -62_135_596_800 && date.timestamp_subsec_nanos() == 0 {
            return None;
        }
        let mut text = date.format("%Y-%m-%dT%H:%M:%S").to_string();
        let nanos = date.timestamp_subsec_nanos();
        if nanos != 0 {
            let fraction = format!("{nanos:09}");
            text.push('.');
            text.push_str(fraction.trim_end_matches('0'));
        }
        text.push('Z');
        Some(text)
    });
    Some((kind, timestamp))
}

fn event_id(identity: &SessionIdentity, record_id: &str, offset: u64) -> String {
    let mut hash = Sha256::new();
    hash.update(b"hmux-codex-task-complete-v1\0");
    hash.update(identity.id.as_bytes());
    hash.update(b"\0");
    hash.update(identity.created_at.to_string().as_bytes());
    hash.update(b"\0");
    hash.update(record_id.as_bytes());
    hash.update(b"\0");
    hash.update(offset.to_string().as_bytes());
    let digest = hash.finalize();
    let mut id = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut id, "{byte:02x}").expect("writing to String");
    }
    id
}

fn bytes(cursor: &Cursor, identity: &SessionIdentity) -> usize {
    cursor.root.as_os_str().as_bytes().len()
        + cursor.path.as_os_str().as_bytes().len()
        + cursor.record_id.len()
        + identity.id.len()
}

impl Tracker {
    pub fn clear(&mut self) {
        self.cursors.clear();
    }

    pub fn observe(
        &mut self,
        observations: &[Observation],
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Vec<Completion>, Error> {
        let result = self.observe_inner(observations, stop, deadline);
        if matches!(result, Err(Error::Cancelled | Error::Limit)) {
            self.clear();
        }
        result
    }

    fn observe_inner(
        &mut self,
        observations: &[Observation],
        stop: &CancellationToken,
        deadline: Instant,
    ) -> Result<Vec<Completion>, Error> {
        check(stop, deadline)?;
        let identities: HashSet<&SessionIdentity> =
            observations.iter().map(|item| &item.identity).collect();
        if identities.len() > CURSORS_MAX {
            return Err(Error::Limit);
        }
        self.cursors
            .retain(|identity, _| identities.contains(identity));
        let mut retained: usize = self
            .cursors
            .iter()
            .map(|(id, cursor)| bytes(cursor, id))
            .sum();
        if retained > RETAINED_MAX {
            return Err(Error::Limit);
        }
        let mut output = Vec::new();
        for item in observations {
            check(stop, deadline)?;
            if hmux_model::validate_session_id(&item.identity.id).is_err()
                || item.identity.created_at < 1
            {
                if let Some(old) = self.cursors.remove(&item.identity) {
                    retained -= bytes(&old, &item.identity);
                }
                continue;
            }
            let Some(binding) = item.binding.as_ref().filter(|binding| {
                binding.provider == Provider::Codex
                    && binding.status == Status::Ready
                    && !binding.path.as_os_str().is_empty()
                    && !binding.root.as_os_str().is_empty()
                    && !binding.record_id.is_empty()
            }) else {
                if let Some(old) = self.cursors.remove(&item.identity) {
                    retained -= bytes(&old, &item.identity);
                }
                continue;
            };
            let output_start = output.len();
            match self.observe_one(&item.identity, binding, stop, deadline, &mut output) {
                Ok(cursor) => {
                    retained += bytes(&cursor, &item.identity);
                    if let Some(old) = self.cursors.insert(item.identity.clone(), cursor) {
                        retained -= bytes(&old, &item.identity);
                    }
                    if retained > RETAINED_MAX {
                        return Err(Error::Limit);
                    }
                }
                Err(Error::Unavailable) => {
                    output.truncate(output_start);
                    if let Some(old) = self.cursors.remove(&item.identity) {
                        retained -= bytes(&old, &item.identity);
                    }
                }
                Err(error) => {
                    output.truncate(output_start);
                    return Err(error);
                }
            }
        }
        check(stop, deadline)?;
        // Output identities also retain bytes until this observation returns.
        let output_bytes: usize = output
            .iter()
            .map(|entry: &Completion| entry.identity.id.len())
            .sum();
        if retained.saturating_add(output_bytes) > RETAINED_MAX {
            return Err(Error::Limit);
        }
        Ok(output)
    }

    fn observe_one(
        &self,
        identity: &SessionIdentity,
        binding: &Binding,
        stop: &CancellationToken,
        deadline: Instant,
        output: &mut Vec<Completion>,
    ) -> Result<Cursor, Error> {
        let mut file = open_record(&binding.root, &binding.path).map_err(|_| Error::Unavailable)?;
        let info = file.metadata().map_err(|_| Error::Unavailable)?;
        let current = self.cursors.get(identity);
        let same = if let Some(cursor) = current {
            let possible = cursor.root == binding.root
                && cursor.path == binding.path
                && cursor.record_id == binding.record_id
                && cursor.dev == info.dev()
                && cursor.ino == info.ino()
                && info.len() >= cursor.offset
                && info.len() - cursor.offset <= TAIL_MAX;
            if possible {
                let (hash, len) = anchor(&mut file, cursor.offset, stop, deadline)?;
                cursor.anchor_len == len && cursor.anchor == hash
            } else {
                false
            }
        } else {
            false
        };
        let start = if same {
            current.expect("same implies cursor").offset
        } else {
            info.len().saturating_sub(TAIL_MAX)
        };
        let mut armed = if same {
            current.expect("same implies cursor").armed
        } else {
            false
        };
        let mut found = Vec::new();
        let remaining = EMITTED_MAX.saturating_sub(output.len());
        let offset = scan(
            &mut file,
            start,
            info.len(),
            !same,
            stop,
            deadline,
            |line, position| {
                if let Some((kind, timestamp)) = event(line) {
                    if kind == "task_started" {
                        armed = true;
                    } else {
                        if same && armed && found.len() < remaining {
                            if let Some(completed_at) = timestamp {
                                found.push(Completion {
                                    identity: identity.clone(),
                                    id: event_id(identity, &binding.record_id, position),
                                    completed_at,
                                });
                            }
                        }
                        armed = false;
                    }
                }
            },
        )?;
        let (anchor, anchor_len) = anchor(&mut file, offset, stop, deadline)?;
        output.extend(found);
        Ok(Cursor {
            root: binding.root.clone(),
            path: binding.path.clone(),
            record_id: binding.record_id.clone(),
            dev: info.dev(),
            ino: info.ino(),
            offset,
            anchor,
            anchor_len,
            armed,
        })
    }
}

#[cfg(test)]
#[path = "completion_tracker_tests.rs"]
mod tests;
