use super::*;
use serde::Deserialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    root: PathBuf,
    path: PathBuf,
    observation: Observation,
    tracker: Tracker,
    stop: CancellationToken,
}

impl Fixture {
    fn new(initial: &[&str]) -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let dir = base.join(format!(
            "hmux-completion-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = dir.join("sessions");
        let path = root.join("rollout-test.jsonl");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, b"{\"type\":\"session_meta\"}\n").unwrap();
        let mut fixture = Self {
            dir,
            root: root.clone(),
            path: path.clone(),
            observation: Observation {
                identity: SessionIdentity {
                    id: "$1".into(),
                    created_at: 42,
                },
                binding: Some(Arc::new(Binding {
                    provider: Provider::Codex,
                    provider_pid: 20,
                    file_pid: 20,
                    path,
                    root,
                    record_id: "test".into(),
                    model: String::new(),
                    state: String::new(),
                    working_since: 0,
                    status: Status::Ready,
                })),
            },
            tracker: Tracker::default(),
            stop: CancellationToken::new(),
        };
        for line in initial {
            fixture.append(line);
        }
        fixture
    }

    fn append(&mut self, line: &str) {
        let mut file = OpenOptions::new().append(true).open(&self.path).unwrap();
        writeln!(file, "{line}").unwrap();
    }

    fn raw_append(&mut self, data: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap()
            .write_all(data)
            .unwrap();
    }

    fn observe(&mut self) -> Vec<Completion> {
        self.tracker
            .observe(
                &[self.observation.clone()],
                &self.stop,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.dir).unwrap();
    }
}

const START: &str =
    r#"{"timestamp":"2026-09-20T01:02:00Z","type":"event_msg","payload":{"type":"task_started"}}"#;
const COMPLETE: &str = r#"{"timestamp":"2026-09-20T01:02:03.456Z","type":"event_msg","payload":{"type":"task_complete"}}"#;

#[derive(Deserialize)]
struct Oracle {
    events: Vec<OracleEvent>,
    ids: Vec<OracleID>,
}
#[derive(Deserialize)]
struct OracleEvent {
    name: String,
    line: String,
    kind: String,
    completed_at: String,
}
#[derive(Deserialize)]
struct OracleID {
    session_id: String,
    created_at: i64,
    record_id: String,
    offset: u64,
    id: String,
}

#[test]
fn go_completion_oracle() {
    let fixture: Oracle = serde_json::from_str(include_str!(
        "../../../tests/fixtures/completion-v1/go-oracle.json"
    ))
    .unwrap();
    assert!((15..=25).contains(&fixture.events.len()));
    for case in fixture.events {
        let actual = event(case.line.as_bytes());
        assert_eq!(
            actual.as_ref().map_or("", |(kind, _)| *kind),
            case.kind,
            "{} kind",
            case.name
        );
        assert_eq!(
            actual
                .and_then(|(_, timestamp)| timestamp)
                .unwrap_or_default(),
            case.completed_at,
            "{} timestamp",
            case.name
        );
    }
    for case in fixture.ids {
        assert_eq!(
            event_id(
                &SessionIdentity {
                    id: case.session_id,
                    created_at: case.created_at
                },
                &case.record_id,
                case.offset
            ),
            case.id
        );
    }
}

#[test]
fn go_event_id_vector() {
    assert_eq!(
        event_id(
            &SessionIdentity {
                id: "$1".into(),
                created_at: 42
            },
            "test",
            123
        ),
        "43ae4bdb76b5ba6ac8289660f79cc43c37a429e45c43532398cbb84d50845f75"
    );
}

#[test]
fn running_baseline_and_stable_id() {
    let mut f = Fixture::new(&[START]);
    assert!(f.observe().is_empty());
    let offset = fs::metadata(&f.path).unwrap().len();
    f.append(COMPLETE);
    let result = f.observe();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].identity, f.observation.identity);
    assert_eq!(result[0].completed_at, "2026-09-20T01:02:03.456Z");
    assert_eq!(
        result[0].id,
        event_id(&f.observation.identity, "test", offset)
    );
    assert!(f.observe().is_empty());
}

#[test]
fn fast_turn_historical_and_missing_timestamp() {
    let mut f = Fixture::new(&[START, COMPLETE]);
    assert!(f.observe().is_empty());
    f.append(START);
    f.append(COMPLETE);
    assert_eq!(f.observe().len(), 1);
    f.append(START);
    f.append(r#"{"type":"task_complete"}"#);
    assert!(f.observe().is_empty());
}

#[test]
fn waits_for_complete_line() {
    let mut f = Fixture::new(&[START]);
    f.observe();
    f.raw_append(COMPLETE.as_bytes());
    assert!(f.observe().is_empty());
    f.raw_append(b"\n");
    assert_eq!(f.observe().len(), 1);
}

#[test]
fn rebind_unknown_identity_and_reset() {
    let mut f = Fixture::new(&[START]);
    f.observe();
    f.append(COMPLETE);
    let other = f.root.join("other.jsonl");
    fs::copy(&f.path, &other).unwrap();
    let mut binding = (**f.observation.binding.as_ref().unwrap()).clone();
    binding.path = other.clone();
    binding.record_id = "other".into();
    f.observation.binding = Some(Arc::new(binding));
    assert!(f.observe().is_empty());
    f.observation.identity.created_at = 43;
    f.append(START);
    assert!(f.observe().is_empty());
    f.observation.binding = None;
    assert!(f.observe().is_empty());
    f.observation.binding = Some(Arc::new(Binding {
        provider: Provider::Codex,
        provider_pid: 20,
        file_pid: 20,
        path: other,
        root: f.root.clone(),
        record_id: "other".into(),
        model: String::new(),
        state: String::new(),
        working_since: 0,
        status: Status::Ready,
    }));
    assert!(f.observe().is_empty());
    f.tracker.clear();
    assert!(f.observe().is_empty());
}

#[test]
fn rewrite_anchor_and_shrink_rebaseline() {
    let mut f = Fixture::new(&[START]);
    f.observe();
    let original = fs::read(&f.path).unwrap();
    let mut changed = original.clone();
    changed[20] = b'x';
    fs::write(&f.path, changed).unwrap();
    f.append(COMPLETE);
    assert!(f.observe().is_empty());
    fs::write(&f.path, b"{}\n").unwrap();
    f.append(START);
    f.append(COMPLETE);
    assert!(f.observe().is_empty());
    f.append(COMPLETE);
    assert_eq!(f.observe().len(), 0);
    f.append(START);
    f.append(COMPLETE);
    assert_eq!(f.observe().len(), 1);
}

#[test]
fn output_cap_advances_cursor() {
    let mut f = Fixture::new(&[]);
    f.observe();
    for _ in 0..70 {
        f.append(START);
        f.append(COMPLETE);
    }
    assert_eq!(f.observe().len(), EMITTED_MAX);
    assert!(f.observe().is_empty());
}

#[test]
fn oversized_line_is_skipped_without_losing_following_events() {
    let mut f = Fixture::new(&[]);
    f.observe();
    f.raw_append(&vec![b'x'; LINE_MAX + 1]);
    f.raw_append(b"\n");
    f.append(START);
    f.append(COMPLETE);
    assert_eq!(f.observe().len(), 1);
}

#[test]
fn oversized_tail_baselines_at_eof() {
    let mut f = Fixture::new(&[]);
    f.raw_append(&vec![b'x'; TAIL_MAX as usize + 1]);
    assert!(f.observe().is_empty());
    f.raw_append(b"\n");
    f.append(START);
    f.append(COMPLETE);
    assert_eq!(f.observe().len(), 1);
}

#[test]
fn cancellation_and_limit_clear_state() {
    let mut f = Fixture::new(&[START]);
    f.observe();
    f.append(COMPLETE);
    f.stop.cancel();
    assert_eq!(
        f.tracker.observe(
            &[f.observation.clone()],
            &f.stop,
            Instant::now() + Duration::from_secs(5),
        ),
        Err(Error::Cancelled)
    );
    assert!(f.tracker.cursors.is_empty());
    f.stop = CancellationToken::new();
    assert!(f.observe().is_empty());
    let many: Vec<_> = (0..=CURSORS_MAX)
        .map(|n| Observation {
            identity: SessionIdentity {
                id: format!("${n}"),
                created_at: 42,
            },
            binding: None,
        })
        .collect();
    assert_eq!(
        f.tracker
            .observe(&many, &f.stop, Instant::now() + Duration::from_secs(5)),
        Err(Error::Limit)
    );
    assert!(f.tracker.cursors.is_empty());
}
