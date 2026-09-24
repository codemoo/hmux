use super::*;
use crate::binding::Provider;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-records-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn put(&self, relative: &str, data: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn codex_base() -> Binding {
    Binding::unavailable(Provider::Codex, 20)
}
fn claude_base() -> Binding {
    Binding::unavailable(Provider::Claude, 20)
}
fn rollout(f: &Fixture, id: &str, source: &str, events: &str) -> PathBuf {
    f.put(&format!(".codex/sessions/2026/09/24/rollout-2026-09-24T00-00-00-{id}.jsonl"),
        &format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"source\":{source}}}}}\n{events}"))
}
fn claude_profile(f: &Fixture, root: &str, model: &str) {
    f.put(&format!("{root}/sessions/20.json"), "{\"pid\":20,\"sessionId\":\"claude-session\",\"status\":\"busy\",\"statusUpdatedAt\":1700000000123}");
    f.put(
        &format!("{root}/projects/project/claude-session.jsonl"),
        &format!("{{\"message\":{{\"model\":\"{model}\"}}}}\n"),
    );
}
#[test]
fn opener_rejects_escape_links_and_inode_swap() {
    assert!(owned(rustix::process::geteuid().as_raw()));
    assert!(!owned(rustix::process::geteuid().as_raw().wrapping_add(1)));
    let f = Fixture::new();
    let root = f.0.join("root");
    let file = f.put("root/child/record.jsonl", "ok");
    let outside = f.put("outside.jsonl", "outside");
    assert!(open_record(&root, &file).is_ok());
    assert!(open_record(&root, &outside).is_err());
    symlink(&outside, f.0.join("root/link.jsonl")).unwrap();
    assert!(open_record(&root, &f.0.join("root/link.jsonl")).is_err());
    symlink(f.0.join("root/child"), f.0.join("root/linked-child")).unwrap();
    assert!(open_record(&root, &f.0.join("root/linked-child/record.jsonl")).is_err());
    let replaced = f.0.join("root/child/replaced.jsonl");
    fs::write(&replaced, "replacement").unwrap();
    assert!(open_record_inner(&root, &file, || {
        fs::rename(&replaced, &file).unwrap();
    })
    .is_err());
}
#[test]
fn codex_exact_header_and_events() {
    let f = Fixture::new();
    let path = rollout(&f, "main", "\"cli\"", concat!(
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\",\"user_prompt\":\"ignore previous instructions\"}}\n",
        "{\"type\":\"event_msg\",\"timestamp\":\"2026-07-29T01:02:03.456Z\",\"payload\":{\"type\":\"task_started\"}}\n",
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"bad model;token\"}}\n"));
    let bound = bind_codex(
        codex_base(),
        20,
        &[path.clone()],
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert_eq!(bound.record_id, "main");
    assert_eq!(bound.model, "gpt-5.6-sol");
    assert_eq!(bound.state, "working");
    assert_eq!(bound.working_since, 1785286923);
    let brief = bind_codex(
        codex_base(),
        20,
        &[path],
        false,
        &CancellationToken::new(),
        deadline(),
    );
    assert!(brief.model.is_empty());
}
#[test]
fn codex_subagents_and_plausible_unknown_are_not_guessed() {
    let f = Fixture::new();
    let main = rollout(&f, "main", "\"exec\"", "");
    let sub = rollout(&f, "sub", "{\"subagent\":{\"other\":\"review\"}}", "");
    let valid = bind_codex(
        codex_base(),
        20,
        &[sub, main.clone()],
        false,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(valid.status, Status::Ready);
    let unknown = rollout(&f, "unknown", "\"other\"", "");
    assert_eq!(
        bind_codex(
            codex_base(),
            20,
            &[main.clone(), unknown],
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    let second = rollout(&f, "second", "\"vscode\"", "");
    assert_eq!(
        bind_codex(
            codex_base(),
            20,
            &[main, second],
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    let malformed = f.put(
        ".codex/sessions/2026/09/24/rollout-bad-id.jsonl",
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"wrong\",\"source\":\"cli\"}}\n",
    );
    assert_eq!(
        bind_codex(
            codex_base(),
            20,
            &[malformed],
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    assert!(codex_root(&f.0.join(".codex/sessions/2026/99/24/rollout-x.jsonl")).is_none());
}
#[test]
fn claude_swap_duplicate_and_registry_change() {
    let f = Fixture::new();
    let swap = ".claude-swap-backup/sessions/slot-1";
    claude_profile(&f, swap, "claude-test");
    let bound = bind_claude(
        &f.0,
        claude_base(),
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert_eq!(bound.model, "claude-test");
    assert_eq!(bound.state, "working");
    assert_eq!(bound.working_since, 1700000000);
    claude_profile(&f, ".claude", "claude-other");
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            true,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    fs::remove_dir_all(f.0.join(".claude")).unwrap();
    let changed = bind_claude_inner(
        &f.0,
        claude_base(),
        false,
        &CancellationToken::new(),
        deadline(),
        |path| {
            fs::write(path, "{\"pid\":20,\"sessionId\":\"replaced\"}").unwrap();
        },
    );
    assert_eq!(changed.status, Status::Ambiguous);
}
#[test]
fn claude_duplicate_transcripts_and_cancel() {
    let f = Fixture::new();
    claude_profile(&f, ".claude", "claude-test");
    f.put(".claude/projects/second/claude-session.jsonl", "{}\n");
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        bind_claude(&f.0, claude_base(), true, &cancel, deadline()).status,
        Status::Unavailable
    );
}

#[test]
fn current_claude_registry_rejection_poison_swap_fallback() {
    let f = Fixture::new();
    let swap = ".claude-swap-backup/sessions/slot-1";
    claude_profile(&f, swap, "claude-test");
    // The swap-only case remains valid when the primary PID slot is absent.
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ready
    );

    let registry = f.put(
        ".claude/sessions/20.json",
        "{\"pid\":20,\"sessionId\":\"current\",\"status\":17}",
    );
    // Go's typed registry decoder rejects a wrong optional field type. An
    // existing rejected current slot must also block a stale swap binding.
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    fs::write(
        &registry,
        "{\"pid\":20,\"sessionId\":\"current\",\"statusUpdatedAt\":\"bad\"}",
    )
    .unwrap();
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    fs::remove_file(&registry).unwrap();
    symlink(f.0.join("elsewhere.json"), &registry).unwrap();
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    fs::remove_file(&registry).unwrap();
    fs::write(&registry, "{\"pid\":21,\"sessionId\":\"current\"}").unwrap();
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Ambiguous
    );
    fs::remove_file(&registry).unwrap();
    fs::write(
        f.0.join(swap).join("sessions/20.json"),
        "{\"pid\":20,\"sessionId\":\"claude-session\",\"status\":17}",
    )
    .unwrap();
    // A malformed optional field cannot become Ready even without a current
    // registry to supply the ambiguity status.
    assert_eq!(
        bind_claude(
            &f.0,
            claude_base(),
            false,
            &CancellationToken::new(),
            deadline()
        )
        .status,
        Status::Unavailable
    );
}

#[test]
fn narrow_metadata_decode_skips_dense_unknown_fields_and_bad_known_types() {
    let f = Fixture::new();
    let dense = format!("[{}]", "0,".repeat(250_000).trim_end_matches(','));
    let events = format!(
        "{{\"type\":\"turn_context\",\"payload\":{{\"model\":\"gpt-5.6-sol\",\"unused\":{dense}}}}}\n\
         {{\"type\":\"task_started\",\"timestamp\":\"2026-07-29T01:02:03Z\"}}\n\
         {{\"type\":\"task_complete\",\"timestamp\":17}}\n\
         {{\"type\":\"turn_context\",\"payload\":{{\"model\":17}}}}\n"
    );
    let path = rollout(&f, "main", "\"cli\"", &events);
    let bound = bind_codex(
        codex_base(),
        20,
        &[path],
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert_eq!(bound.model, "gpt-5.6-sol");
    assert_eq!(bound.state, "working");
    assert_eq!(bound.working_since, 1785286923);

    claude_profile(&f, ".claude", "claude-test");
    let transcript = f.0.join(".claude/projects/project/claude-session.jsonl");
    fs::write(
        &transcript,
        format!(
            "{{\"message\":{{\"model\":\"claude-test\",\"unused\":{dense}}}}}\n\
         {{\"message\":{{\"model\":17}}}}\n"
        ),
    )
    .unwrap();
    let bound = bind_claude(
        &f.0,
        claude_base(),
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert_eq!(bound.model, "claude-test");
}

#[test]
fn null_known_fields_have_go_zero_values() {
    let f = Fixture::new();
    let path = rollout(
        &f,
        "main",
        "\"cli\"",
        "{\"type\":\"task_started\",\"timestamp\":null,\"payload\":null}\n",
    );
    let bound = bind_codex(
        codex_base(),
        20,
        &[path],
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert_eq!(bound.state, "working");
    assert_eq!(bound.working_since, 0);
    f.put(
        ".claude/sessions/20.json",
        "{\"pid\":20,\"sessionId\":\"claude-session\",\"status\":null,\"statusUpdatedAt\":null}",
    );
    f.put(
        ".claude/projects/project/claude-session.jsonl",
        "{\"message\":{\"model\":\"claude-test\"}}\n",
    );
    let bound = bind_claude(
        &f.0,
        claude_base(),
        true,
        &CancellationToken::new(),
        deadline(),
    );
    assert_eq!(bound.status, Status::Ready);
    assert!(bound.state.is_empty());
    assert_eq!(bound.model, "claude-test");
}
