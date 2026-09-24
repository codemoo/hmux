//! Synchronous, bounded session metadata and visibility state.
//!
//! Each update holds the corresponding Go-compatible flock through its read,
//! lifetime check and atomic replacement. This module never invokes tmux.

use hmux_core::PrivateDir;
use hmux_model::{
    safe_text, validate_session_id, validate_stable_id, Catalog, Profile, Session, SessionIdentity,
};
use serde::{
    de::{value::MapAccessDeserializer, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt, io,
    marker::PhantomData,
    path::PathBuf,
    thread,
    time::{Duration, Instant, SystemTime},
};
use tokio_util::sync::CancellationToken;

const METADATA_LIMIT: usize = 8 * 1024 * 1024;
const VISIBILITY_LIMIT: usize = 2 * 1024 * 1024;
const ENTRY_LIMIT: usize = 10_000;
const LOCK_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Unavailable,
    Cancelled,
    Changed,
    Busy,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Invalid => "invalid session state",
            Self::Unavailable => "session state unavailable",
            Self::Cancelled => "session state operation cancelled",
            Self::Changed => "session identity changed",
            Self::Busy => "session state lock busy",
        };
        f.write_str(text)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug)]
pub struct Store {
    state_dir: PathBuf,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct MetadataEntry {
    #[serde(deserialize_with = "null_string")]
    id: String,
    #[serde(deserialize_with = "null_string")]
    name: String,
    created_at: i64,
    #[serde(
        deserialize_with = "null_string",
        skip_serializing_if = "String::is_empty"
    )]
    alias: String,
    #[serde(
        deserialize_with = "null_string",
        skip_serializing_if = "String::is_empty"
    )]
    profile: String,
    #[serde(
        deserialize_with = "null_string",
        skip_serializing_if = "String::is_empty"
    )]
    label: String,
    #[serde(
        deserialize_with = "bounded_tags",
        skip_serializing_if = "Vec::is_empty"
    )]
    tags: Vec<String>,
}

fn null_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

struct ObjectOnly<T>(T);
impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectOnly<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = ObjectOnly<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(ObjectOnly)
            }
        }
        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

fn bounded_map<'de, D: Deserializer<'de>, V: Deserialize<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, V>, D::Error> {
    struct MapVisitor<V>(PhantomData<V>);
    impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a bounded session map")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if values.len() >= ENTRY_LIMIT
                    || values.contains_key(&key)
                    || validate_session_id(&key).is_err()
                {
                    return Err(serde::de::Error::custom("invalid session map"));
                }
                let value = map.next_value::<ObjectOnly<V>>()?.0;
                values.insert(key, value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(MapVisitor(PhantomData))
}

fn bounded_tags<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    struct Tags(Vec<String>);
    impl<'de> Deserialize<'de> for Tags {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct TagsVisitor;
            impl<'de> Visitor<'de> for TagsVisitor {
                type Value = Tags;
                fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    f.write_str("a bounded tag array")
                }
                fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
                    let mut tags = Vec::new();
                    while let Some(tag) = seq.next_element::<Option<String>>()? {
                        if tags.len() >= 64 {
                            return Err(serde::de::Error::custom("too many tags"));
                        }
                        tags.push(tag.unwrap_or_default());
                    }
                    Ok(Tags(tags))
                }
            }
            deserializer.deserialize_seq(TagsVisitor)
        }
    }
    Ok(Option::<Tags>::deserialize(deserializer)?.map_or_else(Vec::new, |tags| tags.0))
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MetadataState {
    #[serde(default)]
    version: u32,
    #[serde(deserialize_with = "bounded_map")]
    sessions: BTreeMap<String, MetadataEntry>,
    #[serde(default, deserialize_with = "null_string")]
    updated_at: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct VisibilityEntry {
    #[serde(deserialize_with = "null_string")]
    id: String,
    #[serde(deserialize_with = "null_string")]
    name: String,
    created_at: i64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VisibilityState {
    #[serde(default)]
    version: u32,
    #[serde(deserialize_with = "bounded_map")]
    hidden: BTreeMap<String, VisibilityEntry>,
    #[serde(default, deserialize_with = "null_string")]
    updated_at: String,
}

fn check(cancel: &CancellationToken, deadline: Instant) -> Result<(), Error> {
    if cancel.is_cancelled() || Instant::now() >= deadline {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn valid_text(value: &str, max: usize) -> bool {
    safe_text(value, max) == value
}
fn validate_identity(id: &str, name: &str, created_at: i64) -> Result<(), Error> {
    if validate_session_id(id).is_err() || created_at < 1 || !valid_text(name, 512) {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn validate_alias(alias: &str) -> Result<(), Error> {
    if alias.len() > 128 || !valid_text(alias, 128) {
        Err(Error::Invalid)
    } else {
        Ok(())
    }
}
fn validate_metadata(entry: &MetadataEntry) -> Result<(), Error> {
    validate_identity(&entry.id, &entry.name, entry.created_at)?;
    validate_alias(&entry.alias)?;
    if (!entry.profile.is_empty() && validate_stable_id(&entry.profile).is_err())
        || !valid_text(&entry.label, 256)
        || entry.tags.len() > 64
        || entry.tags.iter().any(|tag| !valid_text(tag, 128))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn validate_visibility(entry: &VisibilityEntry) -> Result<(), Error> {
    validate_identity(&entry.id, &entry.name, entry.created_at)
}
fn updated_at() -> String {
    chrono::DateTime::<chrono::Utc>::from(SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)
}

fn parse_metadata(data: &[u8]) -> Result<MetadataState, Error> {
    let state: MetadataState = serde_json::from_slice::<ObjectOnly<MetadataState>>(data)
        .map_err(|_| Error::Invalid)?
        .0;
    if state.version != 1
        || state.sessions.len() > ENTRY_LIMIT
        || state
            .sessions
            .iter()
            .any(|(key, entry)| key != &entry.id || validate_metadata(entry).is_err())
    {
        return Err(Error::Invalid);
    }
    Ok(state)
}
fn parse_visibility(data: &[u8]) -> Result<VisibilityState, Error> {
    let state: VisibilityState = serde_json::from_slice::<ObjectOnly<VisibilityState>>(data)
        .map_err(|_| Error::Invalid)?
        .0;
    if state.version != 1
        || state.hidden.len() > ENTRY_LIMIT
        || state
            .hidden
            .iter()
            .any(|(key, entry)| key != &entry.id || validate_visibility(entry).is_err())
    {
        return Err(Error::Invalid);
    }
    Ok(state)
}

impl Store {
    /// Construct without touching the filesystem.
    pub fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    fn read_dir(&self) -> Result<Option<PrivateDir>, Error> {
        match PrivateDir::open(&self.state_dir.join("sessions")) {
            Ok(dir) => Ok(Some(dir)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn write_dir(&self) -> Result<PrivateDir, Error> {
        PrivateDir::open_or_create_trusted(&self.state_dir)
            .and_then(|dir| dir.create_private_child(OsStr::new("sessions")))
            .map_err(|_| Error::Unavailable)
    }
    fn read_metadata(dir: &PrivateDir) -> Result<Option<MetadataState>, Error> {
        match dir.read_private(OsStr::new("sessions.json"), METADATA_LIMIT) {
            Ok(data) => parse_metadata(&data).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn read_visibility(dir: &PrivateDir) -> Result<Option<VisibilityState>, Error> {
        match dir.read_private(OsStr::new("session-visibility.json"), VISIBILITY_LIMIT) {
            Ok(data) => parse_visibility(&data).map(Some),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(Error::Unavailable),
        }
    }
    fn lock(
        dir: &PrivateDir,
        name: &str,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<hmux_core::FileLock, Error> {
        let until = Instant::now() + LOCK_WAIT;
        loop {
            check(cancel, deadline)?;
            if let Some(lock) = dir
                .try_lock(OsStr::new(name))
                .map_err(|_| Error::Unavailable)?
            {
                check(cancel, deadline)?;
                return Ok(lock);
            }
            if Instant::now() >= until {
                return Err(Error::Busy);
            }
            thread::sleep(
                POLL.min(until.saturating_duration_since(Instant::now()))
                    .min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    pub fn apply(&self, catalog: &mut Catalog) -> Result<(), Error> {
        let Some(dir) = self.read_dir()? else {
            return Ok(());
        };
        let Some(state) = Self::read_metadata(&dir)? else {
            return Ok(());
        };
        for session in catalog.sessions.iter_mut().flatten() {
            if let Some(entry) = state.sessions.get(&session.id).filter(|entry| {
                entry.name == session.name && entry.created_at == session.created_at
            }) {
                session.alias.clone_from(&entry.alias);
                session.profile.clone_from(&entry.profile);
                session.label.clone_from(&entry.label);
                session.tags = (!entry.tags.is_empty()).then(|| entry.tags.clone());
            }
        }
        Ok(())
    }
    pub fn apply_visibility(&self, catalog: &mut Catalog) -> Result<(), Error> {
        let Some(dir) = self.read_dir()? else {
            return Ok(());
        };
        let Some(state) = Self::read_visibility(&dir)? else {
            return Ok(());
        };
        for session in catalog.sessions.iter_mut().flatten() {
            session.hidden = state.hidden.get(&session.id).is_some_and(|entry| {
                entry.name == session.name && entry.created_at == session.created_at
            });
        }
        Ok(())
    }

    fn update_metadata(
        &self,
        cancel: &CancellationToken,
        deadline: Instant,
        action: impl FnOnce(&mut MetadataState) -> Result<(), Error>,
    ) -> Result<(), Error> {
        check(cancel, deadline)?;
        let dir = self.write_dir()?;
        check(cancel, deadline)?;
        let _lock = Self::lock(&dir, "sessions.lock", cancel, deadline)?;
        let mut state = Self::read_metadata(&dir)?.unwrap_or_else(|| MetadataState {
            version: 1,
            ..MetadataState::default()
        });
        check(cancel, deadline)?;
        action(&mut state)?;
        check(cancel, deadline)?;
        if state.sessions.len() > ENTRY_LIMIT
            || state
                .sessions
                .iter()
                .any(|(key, entry)| key != &entry.id || validate_metadata(entry).is_err())
        {
            return Err(Error::Invalid);
        }
        state.version = 1;
        state.updated_at = updated_at();
        let mut data = serde_json::to_vec(&state).map_err(|_| Error::Unavailable)?;
        data.push(b'\n');
        if data.len() > METADATA_LIMIT {
            return Err(Error::Invalid);
        }
        check(cancel, deadline)?;
        // Once atomic persistence begins, report its outcome. Cancellation
        // after rename/sync cannot undo a committed update.
        dir.write_atomic_private(OsStr::new("sessions.json"), &data)
            .map_err(|_| Error::Unavailable)
    }
    fn update_visibility(
        &self,
        cancel: &CancellationToken,
        deadline: Instant,
        action: impl FnOnce(&mut VisibilityState) -> Result<(), Error>,
    ) -> Result<(), Error> {
        check(cancel, deadline)?;
        let dir = self.write_dir()?;
        check(cancel, deadline)?;
        let _lock = Self::lock(&dir, "visibility.lock", cancel, deadline)?;
        let mut state = Self::read_visibility(&dir)?.unwrap_or_else(|| VisibilityState {
            version: 1,
            ..VisibilityState::default()
        });
        check(cancel, deadline)?;
        action(&mut state)?;
        check(cancel, deadline)?;
        if state.hidden.len() > ENTRY_LIMIT
            || state
                .hidden
                .iter()
                .any(|(key, entry)| key != &entry.id || validate_visibility(entry).is_err())
        {
            return Err(Error::Invalid);
        }
        state.version = 1;
        state.updated_at = updated_at();
        let mut data = serde_json::to_vec(&state).map_err(|_| Error::Unavailable)?;
        data.push(b'\n');
        if data.len() > VISIBILITY_LIMIT {
            return Err(Error::Invalid);
        }
        check(cancel, deadline)?;
        // Once atomic persistence begins, report its outcome. Cancellation
        // after rename/sync cannot undo a committed update.
        dir.write_atomic_private(OsStr::new("session-visibility.json"), &data)
            .map_err(|_| Error::Unavailable)
    }

    pub fn set_profile(
        &self,
        session: &Session,
        profile: &Profile,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        validate_identity(&session.id, &session.name, session.created_at)?;
        if validate_stable_id(&profile.id).is_err()
            || !valid_text(&profile.label, 256)
            || profile
                .tags
                .as_ref()
                .is_some_and(|tags| tags.len() > 64 || tags.iter().any(|tag| !valid_text(tag, 128)))
        {
            return Err(Error::Invalid);
        }
        self.update_metadata(&cancel, deadline, |state| {
            let mut entry = state
                .sessions
                .remove(&session.id)
                .filter(|entry| {
                    entry.name == session.name && entry.created_at == session.created_at
                })
                .unwrap_or_default();
            entry.id.clone_from(&session.id);
            entry.name.clone_from(&session.name);
            entry.created_at = session.created_at;
            entry.profile.clone_from(&profile.id);
            entry.label.clone_from(&profile.label);
            entry.tags = profile.tags.clone().unwrap_or_default();
            state.sessions.insert(session.id.clone(), entry);
            Ok(())
        })
    }

    pub fn set_alias_expected<F>(
        &self,
        expected: &SessionIdentity,
        alias: &str,
        cancel: CancellationToken,
        deadline: Instant,
        resolve: F,
    ) -> Result<(), Error>
    where
        F: FnOnce() -> Result<Session, Error>,
    {
        if validate_session_id(&expected.id).is_err() || expected.created_at < 1 {
            return Err(Error::Invalid);
        }
        let alias = alias.trim();
        validate_alias(alias)?;
        self.update_metadata(&cancel, deadline, |state| {
            check(&cancel, deadline)?;
            let session = resolve()?;
            check(&cancel, deadline)?;
            if session.id != expected.id || session.created_at != expected.created_at {
                return Err(Error::Changed);
            }
            validate_identity(&session.id, &session.name, session.created_at)?;
            let mut entry = state
                .sessions
                .remove(&session.id)
                .filter(|entry| {
                    entry.name == session.name && entry.created_at == session.created_at
                })
                .unwrap_or_default();
            entry.id.clone_from(&session.id);
            entry.name.clone_from(&session.name);
            entry.created_at = session.created_at;
            entry.alias = alias.to_owned();
            state.sessions.insert(session.id.clone(), entry);
            Ok(())
        })
    }

    pub fn set_hidden_expected<F>(
        &self,
        expected: &SessionIdentity,
        hidden: bool,
        cancel: CancellationToken,
        deadline: Instant,
        resolve: F,
    ) -> Result<(), Error>
    where
        F: FnOnce() -> Result<Session, Error>,
    {
        if validate_session_id(&expected.id).is_err() || expected.created_at < 1 {
            return Err(Error::Invalid);
        }
        self.update_visibility(&cancel, deadline, |state| {
            check(&cancel, deadline)?;
            let session = resolve()?;
            check(&cancel, deadline)?;
            if session.id != expected.id || session.created_at != expected.created_at {
                return Err(Error::Changed);
            }
            validate_identity(&session.id, &session.name, session.created_at)?;
            if hidden {
                state.hidden.insert(
                    session.id.clone(),
                    VisibilityEntry {
                        id: session.id,
                        name: session.name,
                        created_at: session.created_at,
                    },
                );
            } else {
                state.hidden.remove(&session.id);
            }
            Ok(())
        })
    }

    /// Import metadata from a legacy catalog while preserving fields already
    /// recorded for the same lifetime. Intended for one-time local migration.
    pub fn import_legacy(
        &self,
        sessions: &[Session],
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<(), Error> {
        // Reject oversized caller input before allocating or acquiring a lock.
        if sessions.len() > ENTRY_LIMIT {
            return Err(Error::Invalid);
        }
        self.update_metadata(&cancel, deadline, |state| {
            for session in sessions {
                validate_identity(&session.id, &session.name, session.created_at)?;
                let mut entry = state
                    .sessions
                    .remove(&session.id)
                    .filter(|entry| {
                        entry.name == session.name && entry.created_at == session.created_at
                    })
                    .unwrap_or_default();
                entry.id.clone_from(&session.id);
                entry.name.clone_from(&session.name);
                entry.created_at = session.created_at;
                if !session.alias.is_empty() {
                    validate_alias(&session.alias)?;
                    entry.alias.clone_from(&session.alias);
                }
                if !session.profile.is_empty() {
                    if validate_stable_id(&session.profile).is_err() {
                        return Err(Error::Invalid);
                    }
                    entry.profile.clone_from(&session.profile);
                }
                if !session.label.is_empty() {
                    if !valid_text(&session.label, 256) {
                        return Err(Error::Invalid);
                    }
                    entry.label.clone_from(&session.label);
                }
                if let Some(tags) = session.tags.as_ref().filter(|tags| !tags.is_empty()) {
                    entry.tags.clone_from(tags);
                }
                state.sessions.insert(session.id.clone(), entry);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "sessionstate_tests.rs"]
mod tests;
