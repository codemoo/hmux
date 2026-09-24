use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    sync::atomic::{AtomicU64, Ordering},
};
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-workflow-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&p).unwrap();
        Self(p)
    }
    fn store(&self) -> Store {
        Store::new(self.0.clone())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn now() -> DateTime<Utc> {
    "2026-09-24T00:00:00Z".parse().unwrap()
}
fn binding() -> Binding {
    Binding {
        id: "$1".into(),
        created_at: 1700000000,
    }
}
fn catalog() -> Catalog {
    Catalog {
        sessions: Some(vec![hmux_model::Session {
            id: "$1".into(),
            created_at: 1700000000,
            ..Default::default()
        }]),
        ..Default::default()
    }
}
fn event(name: &str) -> HookEvent {
    HookEvent {
        session_id: "synthetic-session".into(),
        turn_id: "synthetic-turn".into(),
        hook_event_name: name.into(),
        agent_id: "synthetic-agent".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn hook_binding_uses_validated_identity_and_joins_cancelled_query() {
    use crate::catalog::TmuxCatalogReader;
    use hmux_core::command::CommandRunner;
    let f = Fixture::new();
    let tmux = f.0.join("fake-tmux");
    fs::write(&tmux, "#!/bin/sh\nroot=${0%/*}\nprintf '%s\\n' \"$@\" > \"$root/args\"\nif [ -f \"$root/slow\" ]; then exec /bin/sleep 30; fi\nprintf '$1\\t1700000000\\n'\n").unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
    let reader = TmuxCatalogReader::new(tmux, None, Duration::from_secs(2)).unwrap();
    let runner = CommandRunner::new(1).unwrap();
    let stop = CancellationToken::new();
    let mut env = BindingEnvironment {
        session_id: "$1".into(),
        created_at: "1700000000".into(),
        pane: "%9".into(),
    };
    assert_eq!(
        resolve_binding(&env, &reader, &runner, &stop)
            .await
            .unwrap(),
        binding()
    );
    env.created_at = "invalid".into();
    assert_eq!(
        resolve_binding(&env, &reader, &runner, &stop).await,
        Err(Error::Invalid)
    );
    assert!(!f.0.join("args").exists());
    env.session_id.clear();
    assert_eq!(
        resolve_binding(&env, &reader, &runner, &stop)
            .await
            .unwrap(),
        binding()
    );
    assert_eq!(
        fs::read_to_string(f.0.join("args")).unwrap(),
        "display-message\n-p\n-t\n%9\n#{session_id}\t#{session_created}\n"
    );
    env.pane = "%9; kill-server".into();
    assert_eq!(
        resolve_binding(&env, &reader, &runner, &stop).await,
        Err(Error::Invalid)
    );
    env.pane = "%9".into();
    fs::write(f.0.join("slow"), "").unwrap();
    fs::remove_file(f.0.join("args")).unwrap();
    let query = resolve_binding(&env, &reader, &runner, &stop);
    let cancellation = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !f.0.join("args").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop.cancel();
    };
    let (result, ()) = tokio::join!(query, cancellation);
    assert_eq!(result, Err(Error::Cancelled));
    assert_eq!(runner.available_slots(), 1);
}

#[test]
fn helper_filter_requires_unambiguous_identity_and_omits_empty_sessions() {
    let mut a = hmux_model::Session {
        id: "$1".into(),
        name: "same".into(),
        ..Default::default()
    };
    let b = hmux_model::Session {
        id: "$2".into(),
        name: "same".into(),
        ..Default::default()
    };
    assert!(views(&[a.clone(), b.clone()], "same")
        .unwrap_err()
        .contains("ambiguous"));
    assert!(views(&[a.clone()], "missing")
        .unwrap_err()
        .contains("does not exist"));
    assert_eq!(views(&[a.clone(), b.clone()], "$2").unwrap()[0].id, "$2");
    assert!(views(&[a.clone()], "").unwrap().is_empty());
    a.workflows = Some(vec![Workflow::default()]);
    assert_eq!(views(&[a, b], "").unwrap().len(), 1);
}
#[tokio::test]
async fn sanitized_lifecycle_survives_restart_and_preserves_birth_identity() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let s = f.store();
    let cancel = CancellationToken::new();
    let event=HookEvent::parse(br#"{"session_id":"synthetic-session","turn_id":"synthetic-turn","hook_event_name":"UserPromptSubmit","model":"test-model","prompt":"NEVER PERSIST THIS","tool_input":{"secret":"NEVER PERSIST THIS"}}"#).unwrap();
    s.record_hook(binding(), event, now(), &cancel)
        .await
        .unwrap();
    for name in ["SubagentStart", "PermissionRequest", "SubagentStop"] {
        s.record_hook(binding(), self::event(name), now(), &cancel)
            .await
            .unwrap();
    }
    let mut c = catalog();
    f.store().apply(&mut c, now(), &cancel).unwrap();
    let summary = c.sessions.as_ref().unwrap()[0].workflow.as_ref().unwrap();
    assert_eq!(summary.waiting_approval, 0);
    assert_eq!(summary.running, 1);
    assert_eq!(summary.completed, 1);
    let raw = fs::read_to_string(f.0.join("workflows/state.json")).unwrap();
    assert!(
        !raw.contains("synthetic-session")
            && !raw.contains("synthetic-agent")
            && !raw.contains("NEVER PERSIST")
    );
    assert!(raw.contains("test-model"));
    s.record_hook(binding(), self::event("Stop"), now(), &cancel)
        .await
        .unwrap();
    let mut c = catalog();
    s.apply(&mut c, now(), &cancel).unwrap();
    assert_eq!(
        c.sessions.unwrap()[0].workflow.as_ref().unwrap().completed,
        2
    );
    let mut c = catalog();
    c.sessions.as_mut().unwrap()[0].created_at += 1;
    s.apply(&mut c, now(), &cancel).unwrap();
    assert!(c.sessions.unwrap()[0].workflow.is_none());
}
#[tokio::test]
async fn stale_and_expired_records_prune_without_new_hook() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let s = f.store();
    let cancel = CancellationToken::new();
    s.record_hook(binding(), event("UserPromptSubmit"), now(), &cancel)
        .await
        .unwrap();
    let mut c = catalog();
    s.apply(
        &mut c,
        now() + chrono::Duration::seconds(STALE + 1),
        &cancel,
    )
    .unwrap();
    assert_eq!(c.sessions.unwrap()[0].workflow.as_ref().unwrap().stale, 1);
    let mut c = catalog();
    s.apply(
        &mut c,
        now() + chrono::Duration::seconds(RETAIN + 1),
        &cancel,
    )
    .unwrap();
    assert!(c.sessions.unwrap()[0].workflow.is_none());
    let state: State =
        serde_json::from_slice(&fs::read(f.0.join("workflows/state.json")).unwrap()).unwrap();
    assert!(state.workflows.is_empty());
}
#[tokio::test]
async fn two_owners_serialize_and_unsafe_targets_or_cancel_fail_closed() {
    let _serial = SERIAL.lock().await;
    let f = Fixture::new();
    let cancel = CancellationToken::new();
    let a = f.store();
    let b = f.store();
    let (x, y) = tokio::join!(
        a.record_report(
            binding(),
            Report {
                task_id: "one".into(),
                status: "running".into()
            },
            now(),
            &cancel
        ),
        b.record_report(
            binding(),
            Report {
                task_id: "two".into(),
                status: "completed".into()
            },
            now(),
            &cancel
        )
    );
    x.unwrap();
    y.unwrap();
    let mut c = catalog();
    a.apply(&mut c, now(), &cancel).unwrap();
    let summary = c.sessions.unwrap()[0].workflow.clone().unwrap();
    assert_eq!((summary.running, summary.completed), (1, 1));
    let dir = PrivateDir::open(&f.0.join("workflows")).unwrap();
    let lock = dir.try_lock(OsStr::new("state.lock")).unwrap().unwrap();
    let operation = a.record_report(
        binding(),
        Report {
            task_id: "blocked".into(),
            status: "running".into(),
        },
        now(),
        &cancel,
    );
    let cancel_task = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(operation, cancel_task);
    assert_eq!(result, Err(Error::Cancelled));
    drop(lock);
    let state = f.0.join("workflows/state.json");
    fs::rename(&state, f.0.join("saved")).unwrap();
    symlink(f.0.join("saved"), &state).unwrap();
    assert_eq!(
        a.record_report(
            binding(),
            Report {
                task_id: "unsafe".into(),
                status: "running".into()
            },
            now(),
            &CancellationToken::new()
        )
        .await,
        Err(Error::Unavailable)
    );
    fs::remove_file(&state).unwrap();
    fs::rename(f.0.join("saved"), &state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        a.apply(&mut catalog(), now(), &CancellationToken::new()),
        Err(Error::Unavailable)
    );
}
#[test]
fn hook_limits_validation_and_missed_subagent_start() {
    assert!(HookEvent::parse(&vec![b' '; MAX_HOOK + 1]).is_err());
    assert!(HookEvent::parse(b"{}").is_err());
    assert!(HookEvent::parse(b"null").is_err());
    let mut state = State {
        version: 1,
        ..Default::default()
    };
    record_hook(
        &mut state,
        &binding(),
        &event("SubagentStop"),
        now().timestamp(),
    )
    .unwrap();
    assert!(state.valid());
    assert_eq!(state.workflows.values().next().unwrap().nodes.len(), 2);
    let mut malformed = serde_json::to_value(&state).unwrap();
    malformed["extra"] = true.into();
    assert!(serde_json::from_value::<State>(malformed).is_err());
}
