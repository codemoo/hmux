use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "hmux-state-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    fn store(&self) -> Store {
        Store::new(self.0.clone())
    }
    fn sessions(&self) -> PathBuf {
        self.0.join("sessions")
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn session(id: &str, name: &str, created_at: i64) -> Session {
    Session {
        id: id.into(),
        name: name.into(),
        created_at,
        ..Session::default()
    }
}
fn identity(session: &Session) -> SessionIdentity {
    SessionIdentity {
        id: session.id.clone(),
        created_at: session.created_at,
    }
}
fn catalog(sessions: Vec<Session>) -> Catalog {
    Catalog {
        sessions: Some(sessions),
        ..Catalog::default()
    }
}
fn file(dir: &TestDir, name: &str) -> Vec<u8> {
    fs::read(dir.sessions().join(name)).unwrap()
}

#[test]
fn missing_reads_have_no_side_effects() {
    let dir = TestDir::new();
    let store = dir.store();
    let mut c = catalog(vec![session("$1", "name", 1)]);
    store.apply(&mut c).unwrap();
    store.apply_visibility(&mut c).unwrap();
    assert!(!dir.sessions().exists());
}

#[test]
fn go_json_fixture_overlays_only_exact_lifetime() {
    let dir = TestDir::new();
    fs::create_dir(dir.sessions()).unwrap();
    fs::set_permissions(dir.sessions(), fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = br#"{"version":1,"sessions":{"$1":{"id":"$1","name":"alpha","created_at":11,"alias":"Nice","profile":"shell","label":"Shell","tags":["dev"]}},"updated_at":"2025-01-01T00:00:00Z"}
"#;
    fs::write(dir.sessions().join("sessions.json"), fixture).unwrap();
    fs::set_permissions(
        dir.sessions().join("sessions.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let fixture = br#"{"version":1,"hidden":{"$1":{"id":"$1","name":"alpha","created_at":11}},"updated_at":"2025-01-01T00:00:00Z"}
"#;
    fs::write(dir.sessions().join("session-visibility.json"), fixture).unwrap();
    fs::set_permissions(
        dir.sessions().join("session-visibility.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut c = catalog(vec![
        session("$1", "alpha", 11),
        session("$1", "alpha", 12),
        session("$1", "renamed", 11),
    ]);
    dir.store().apply(&mut c).unwrap();
    dir.store().apply_visibility(&mut c).unwrap();
    let sessions = c.sessions.unwrap();
    assert_eq!(sessions[0].alias, "Nice");
    assert_eq!(sessions[0].profile, "shell");
    assert_eq!(
        sessions[0].tags.as_deref(),
        Some(["dev".to_owned()].as_slice())
    );
    assert!(sessions[0].hidden);
    assert!(sessions[1].alias.is_empty() && !sessions[1].hidden);
    assert!(sessions[2].alias.is_empty() && !sessions[2].hidden);
}

#[test]
fn profile_preserves_alias_for_lifetime_and_resets_on_reuse() {
    let dir = TestDir::new();
    let store = dir.store();
    let old = session("$1", "alpha", 11);
    store
        .set_alias_expected(
            &identity(&old),
            "  Nice  ",
            CancellationToken::new(),
            deadline(),
            || Ok(old.clone()),
        )
        .unwrap();
    let profile = Profile {
        id: "shell".into(),
        label: "Shell".into(),
        tags: Some(vec!["dev".into()]),
        ..Profile::default()
    };
    store
        .set_profile(&old, &profile, CancellationToken::new(), deadline())
        .unwrap();
    let mut c = catalog(vec![old.clone()]);
    store.apply(&mut c).unwrap();
    assert_eq!(c.sessions.unwrap()[0].alias, "Nice");
    let replacement = session("$1", "alpha", 12);
    store
        .set_profile(&replacement, &profile, CancellationToken::new(), deadline())
        .unwrap();
    let mut c = catalog(vec![replacement]);
    store.apply(&mut c).unwrap();
    assert_eq!(c.sessions.unwrap()[0].alias, "");
}

#[test]
fn stale_resolvers_do_not_commit_or_delete_visibility() {
    let dir = TestDir::new();
    let store = dir.store();
    let live = session("$1", "alpha", 11);
    store
        .set_hidden_expected(
            &identity(&live),
            true,
            CancellationToken::new(),
            deadline(),
            || Ok(live.clone()),
        )
        .unwrap();
    let before = file(&dir, "session-visibility.json");
    let stale = SessionIdentity {
        id: "$1".into(),
        created_at: 10,
    };
    assert_eq!(
        store.set_hidden_expected(&stale, false, CancellationToken::new(), deadline(), || Ok(
            live.clone()
        )),
        Err(Error::Changed)
    );
    assert_eq!(file(&dir, "session-visibility.json"), before);
    assert_eq!(
        store.set_alias_expected(&stale, "old", CancellationToken::new(), deadline(), || Ok(
            live
        )),
        Err(Error::Changed)
    );
    assert!(!dir.sessions().join("sessions.json").exists());
}

#[test]
fn locks_and_cancellation_bound_updates() {
    let dir = TestDir::new();
    let store = dir.store();
    let live = session("$1", "alpha", 11);
    let private = PrivateDir::open(&dir.0)
        .unwrap()
        .create_private_child(OsStr::new("sessions"))
        .unwrap();
    let guard = private
        .try_lock(OsStr::new("sessions.lock"))
        .unwrap()
        .unwrap();
    assert_eq!(
        store.set_alias_expected(
            &identity(&live),
            "x",
            CancellationToken::new(),
            Instant::now() + Duration::from_millis(70),
            || Ok(live.clone())
        ),
        Err(Error::Cancelled)
    );
    drop(guard);
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        store.set_alias_expected(&identity(&live), "x", cancel, deadline(), || Ok(live)),
        Err(Error::Cancelled)
    );
    assert!(!dir.sessions().join("sessions.json").exists());
}

#[test]
fn rejects_oversized_and_unsafe_files_without_exposing_paths() {
    let dir = TestDir::new();
    let store = dir.store();
    let live = session("$1", "alpha", 11);
    fs::create_dir(dir.sessions()).unwrap();
    fs::set_permissions(dir.sessions(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        dir.sessions().join("sessions.json"),
        vec![b' '; METADATA_LIMIT + 1],
    )
    .unwrap();
    let mut c = catalog(vec![live.clone()]);
    assert_eq!(store.apply(&mut c), Err(Error::Unavailable));
    fs::remove_file(dir.sessions().join("sessions.json")).unwrap();
    symlink("/dev/null", dir.sessions().join("sessions.json")).unwrap();
    assert_eq!(
        store.set_profile(
            &live,
            &Profile {
                id: "shell".into(),
                ..Profile::default()
            },
            CancellationToken::new(),
            deadline()
        ),
        Err(Error::Unavailable)
    );
    assert!(!format!("{}", Error::Unavailable).contains("/private"));
}

#[test]
fn invalid_map_and_entry_limits_are_rejected() {
    let dir = TestDir::new();
    let store = dir.store();
    let mut legacy = Vec::with_capacity(ENTRY_LIMIT + 1);
    for number in 0..=ENTRY_LIMIT {
        legacy.push(session(&format!("${number}"), "a", 1));
    }
    assert_eq!(
        store.import_legacy(&legacy, CancellationToken::new(), deadline()),
        Err(Error::Invalid)
    );
    assert!(!dir.sessions().join("sessions.json").exists());
}

#[test]
fn malformed_json_is_rejected_before_overlay_or_replacement() {
    let mut too_many_tags = String::from("{\"version\":1,\"sessions\":{\"$1\":{\"id\":\"$1\",\"name\":\"a\",\"created_at\":1,\"tags\":[");
    too_many_tags.push_str(&vec!["\"x\""; 65].join(","));
    too_many_tags.push_str("]}}}");
    let malformed = [
        "[]",
        "{\"version\":1}",
        "{\"version\":1,\"sessions\":null}",
        "{\"version\":1,\"sessions\":{\"$1\":[\"$1\",\"a\",1]}}",
        "{\"version\":1,\"sessions\":{\"$1\":{\"id\":\"$1\",\"name\":\"a\",\"created_at\":1},\"$1\":{\"id\":\"$1\",\"name\":\"b\",\"created_at\":2}}}",
        "{\"version\":1,\"sessions\":{},\"sessions\":{}}",
        "{\"version\":1,\"sessions\":{}} trailing",
        &too_many_tags,
    ];
    for raw in malformed {
        assert!(
            matches!(parse_metadata(raw.as_bytes()), Err(Error::Invalid)),
            "{raw}"
        );
    }
    for raw in [
        "[]",
        "{\"version\":1}",
        "{\"version\":1,\"hidden\":null}",
        "{\"version\":1,\"hidden\":{\"$1\":[]}}",
    ] {
        assert!(
            matches!(parse_visibility(raw.as_bytes()), Err(Error::Invalid)),
            "{raw}"
        );
    }
    let dir = TestDir::new();
    let live = session("$1", "alpha", 11);
    let store = dir.store();
    store
        .set_alias_expected(
            &identity(&live),
            "good",
            CancellationToken::new(),
            deadline(),
            || Ok(live.clone()),
        )
        .unwrap();
    let malformed = b"{\"version\":1,\"sessions\":null}";
    fs::write(dir.sessions().join("sessions.json"), malformed).unwrap();
    let profile = Profile {
        id: "shell".into(),
        ..Profile::default()
    };
    assert_eq!(
        store.set_profile(&live, &profile, CancellationToken::new(), deadline()),
        Err(Error::Invalid)
    );
    assert_eq!(file(&dir, "sessions.json"), malformed);
}

#[test]
fn decoding_bounds_maps_and_accepts_go_null_optionals() {
    let mut raw = String::from("{\"version\":1,\"sessions\":{");
    for number in 0..=ENTRY_LIMIT {
        if number != 0 {
            raw.push(',');
        }
        raw.push_str(&format!(
            "\"${number}\":{{\"id\":\"${number}\",\"name\":\"n\",\"created_at\":1}}"
        ));
    }
    raw.push_str("}}");
    assert!(matches!(
        parse_metadata(raw.as_bytes()),
        Err(Error::Invalid)
    ));
    let state = parse_metadata(br#"{"version":1,"sessions":{"$1":{"id":"$1","name":"a","created_at":1,"alias":null,"profile":null,"label":null,"tags":[null,"x"]}},"updated_at":null}"#).unwrap();
    let entry = &state.sessions["$1"];
    assert_eq!(entry.alias, "");
    assert_eq!(entry.tags, ["", "x"]);
}

#[test]
fn cancellation_after_resolver_prevents_commit() {
    let dir = TestDir::new();
    let store = dir.store();
    let live = session("$1", "alpha", 11);
    let cancel = CancellationToken::new();
    let resolver_cancel = cancel.clone();
    assert_eq!(
        store.set_alias_expected(&identity(&live), "new", cancel, deadline(), || {
            resolver_cancel.cancel();
            Ok(live)
        }),
        Err(Error::Cancelled)
    );
    assert!(!dir.sessions().join("sessions.json").exists());
}
