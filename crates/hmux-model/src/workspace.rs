//! Pure shared-tab reconciliation, compatible with Go's sharedworkspace owner.
//! Storage, catalog acquisition, account scope and locking belong to the caller.
use super::{object_wire, validate_session_id, SessionIdentity};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};

pub const MAX_TABS: usize = 32;
pub const MAX_BYTES: usize = 32 << 10;
pub const HISTORY: usize = 64;

fn null_default<'de, D: Deserializer<'de>, T: DeserializeOwned + Default>(
    d: D,
) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Only identities needed for tab reconciliation. Catalog labels, commands,
/// process details and metrics are skipped rather than allocated per poll.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionLineage {
    pub id: String,
    pub created_at: i64,
    pub restored_from: Option<SessionIdentity>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
struct LineageWire {
    #[serde(deserialize_with = "null_default")]
    id: String,
    #[serde(deserialize_with = "null_default")]
    created_at: i64,
    restored_from: Option<SessionIdentity>,
}
impl<'de> Deserialize<'de> for SessionLineage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = object_wire::<D, LineageWire>(d)?.unwrap_or_default();
        Ok(Self {
            id: v.id,
            created_at: v.created_at,
            restored_from: v.restored_from,
        })
    }
}
impl From<&super::Session> for SessionLineage {
    fn from(s: &super::Session) -> Self {
        Self {
            id: s.id.clone(),
            created_at: s.created_at,
            restored_from: s.restored_from.clone(),
        }
    }
}

pub fn decode_catalog(raw: &[u8]) -> Result<Vec<SessionLineage>, Error> {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct CatalogWire {
        sessions: Option<Vec<SessionLineage>>,
    }
    // Current Home wire maximum; parsing stays in the caller's bounded worker.
    if raw.len() > 4 << 20 {
        return Err(Error::Invalid);
    }
    let mut decoder = serde_json::Deserializer::from_slice(raw);
    let value = object_wire::<_, CatalogWire>(&mut decoder)
        .map_err(|_| Error::Invalid)?
        .unwrap_or_default();
    decoder.end().map_err(|_| Error::Invalid)?;
    Ok(value.sessions.unwrap_or_default())
}

object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Snapshot (SnapshotWire) {
        #[serde(skip_serializing_if="String::is_empty", deserialize_with="null_default")]
        pub conflict: String,
        #[serde(skip_serializing_if="Vec::is_empty", deserialize_with="null_default")]
        pub applied: Vec<String>,
        #[serde(deserialize_with="null_default")]
        pub version: i64,
        #[serde(deserialize_with="null_default")]
        pub initialized: bool,
        #[serde(deserialize_with="null_default")]
        pub revision: u64,
        #[serde(deserialize_with="null_default")]
        pub tabs: Vec<SessionIdentity>,
        #[serde(skip_serializing_if="Option::is_none")]
        pub selected: Option<SessionIdentity>,
    }
}
object_model! {
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct Change (ChangeWire) {
        #[serde(deserialize_with="null_default")]
        pub operation_id: String,
        #[serde(deserialize_with="null_default")]
        pub revision: u64,
        #[serde(deserialize_with="null_default")]
        pub base: Vec<SessionIdentity>,
        #[serde(deserialize_with="null_default")]
        pub tabs: Vec<SessionIdentity>,
        #[serde(skip_serializing_if="Option::is_none")]
        pub selected: Option<SessionIdentity>,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    TooManyTabs,
    RevisionExhausted,
}

fn valid_ids(ids: &[SessionIdentity]) -> bool {
    ids.len() <= MAX_TABS
        && ids.iter().enumerate().all(|(i, id)| {
            validate_session_id(&id.id).is_ok()
                && id.created_at > 0
                && !ids[..i].iter().any(|previous| previous.id == id.id)
        })
}
impl Snapshot {
    pub fn empty() -> Self {
        Self {
            version: 1,
            ..Self::default()
        }
    }
    pub fn valid(&self) -> bool {
        matches!(self.conflict.as_str(), "" | "workspace_conflict")
            && self.applied.len() <= HISTORY
            && self.version == 1
            && valid_ids(&self.tabs)
            && self.selected.as_ref().is_none_or(|s| self.tabs.contains(s))
    }
    pub fn decode(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_BYTES {
            return Err(Error::Invalid);
        }
        let state: Self = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
        if !state.valid() {
            return Err(Error::Invalid);
        }
        Ok(state)
    }
}
impl Change {
    pub fn valid(&self) -> bool {
        (16..=80).contains(&self.operation_id.len())
            && self
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && valid_ids(&self.base)
            && valid_ids(&self.tabs)
            && self.selected.as_ref().is_none_or(|s| self.tabs.contains(s))
    }
}

pub fn merge(current: &Snapshot, change: &Change) -> Result<Snapshot, Error> {
    if !current.valid() || !change.valid() {
        return Err(Error::Invalid);
    }
    let mut result = current.clone();
    result.version = 1;
    result.initialized = true;
    // A stale device removes only the tabs it explicitly closed in its own base.
    result
        .tabs
        .retain(|id| !change.base.contains(id) || change.tabs.contains(id));
    for (index, id) in change.tabs.iter().enumerate() {
        if change.base.contains(id) || result.tabs.contains(id) {
            continue;
        }
        result.tabs.retain(|old| old.id != id.id);
        let before = change.tabs[index + 1..]
            .iter()
            .find_map(|next| result.tabs.iter().position(|p| p == next));
        let after = || {
            change.tabs[..index].iter().rev().find_map(|previous| {
                result
                    .tabs
                    .iter()
                    .position(|p| p == previous)
                    .map(|i| i + 1)
            })
        };
        let position = before.or_else(after).unwrap_or(result.tabs.len());
        result.tabs.insert(position, id.clone());
    }
    let old_order = change.base.iter().filter(|id| change.tabs.contains(id));
    let new_order = change.tabs.iter().filter(|id| change.base.contains(id));
    if !old_order.eq(new_order) {
        let mut ordered: Vec<_> = change
            .tabs
            .iter()
            .filter(|id| result.tabs.contains(id))
            .cloned()
            .collect();
        for id in &result.tabs {
            if !ordered.contains(id) {
                ordered.push(id.clone())
            }
        }
        result.tabs = ordered;
    }
    if result.tabs.len() > MAX_TABS {
        return Err(Error::TooManyTabs);
    }
    result.selected = None;
    Ok(result)
}

fn live(id: &SessionIdentity, sessions: &[SessionLineage]) -> bool {
    sessions
        .iter()
        .any(|s| s.id == id.id && s.created_at == id.created_at)
}
fn resolve(id: &SessionIdentity, sessions: &[SessionLineage]) -> SessionIdentity {
    if live(id, sessions) {
        return id.clone();
    }
    // Only a unique authoritative restore lineage can change a lifetime.
    let mut candidates = sessions
        .iter()
        .filter(|s| s.restored_from.as_ref() == Some(id));
    match (candidates.next(), candidates.next()) {
        (Some(s), None) => SessionIdentity {
            id: s.id.clone(),
            created_at: s.created_at,
        },
        _ => id.clone(),
    }
}
fn rebase(ids: &[SessionIdentity], sessions: &[SessionLineage]) -> Vec<SessionIdentity> {
    let mut result = Vec::with_capacity(ids.len());
    for id in ids {
        let resolved = resolve(id, sessions);
        if !result.contains(&resolved) {
            result.push(resolved)
        }
    }
    result
}

pub struct Transition {
    /// Store this value only when `changed` is true. Conflict is response-only.
    pub state: Snapshot,
    pub changed: bool,
    pub conflict: bool,
}
impl Transition {
    pub fn reply(&self) -> Snapshot {
        let mut reply = self.state.clone();
        if self.conflict {
            reply.conflict = "workspace_conflict".into();
        }
        reply
    }
}

pub fn reconcile(
    mut current: Snapshot,
    change: Option<&Change>,
    sessions: &[SessionLineage],
) -> Result<Transition, Error> {
    if !current.valid() {
        return Err(Error::Invalid);
    }
    let before = current.clone();
    current.tabs = rebase(&current.tabs, sessions);
    // Focus remains device-local, including when importing legacy saved state.
    current.selected = None;
    current.conflict.clear();
    let mut conflict = false;
    if let Some(change) = change {
        if !current.applied.contains(&change.operation_id) {
            if !change.valid()
                || change.revision > current.revision
                || current.revision - change.revision > HISTORY as u64
            {
                conflict = true;
            } else {
                let mut rebased = change.clone();
                rebased.base = rebase(&change.base, sessions);
                rebased.tabs = rebase(&change.tabs, sessions);
                rebased.selected = None;
                conflict = rebased
                    .tabs
                    .iter()
                    .any(|id| !rebased.base.contains(id) && !live(id, sessions));
                if !conflict {
                    match merge(&current, &rebased) {
                        Ok(next) => current = next,
                        Err(_) => conflict = true,
                    }
                }
            }
            if !conflict {
                if current.applied.len() == HISTORY {
                    current.applied.remove(0);
                }
                current.applied.push(change.operation_id.clone());
            }
        }
    }
    // A malformed restore lineage must not turn a readable workspace into an
    // invalid file. Keep the same identity contract on the resulting state.
    if !current.valid() {
        return Err(Error::Invalid);
    }
    let changed = current != before;
    if changed {
        current.revision = before
            .revision
            .checked_add(1)
            .ok_or(Error::RevisionExhausted)?;
    }
    Ok(Transition {
        state: current,
        changed,
        conflict,
    })
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;
