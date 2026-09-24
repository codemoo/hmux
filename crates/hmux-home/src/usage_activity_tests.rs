use super::*;
use std::{
    fs,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
};
static TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
async fn test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-usage-activity-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn roots(&self) -> Options {
        let claude = self.0.join("claude");
        let codex = self.0.join("codex");
        fs::create_dir_all(&claude).unwrap();
        fs::create_dir_all(&codex).unwrap();
        Options {
            claude_projects_root: claude,
            codex_sessions_root: codex,
            claude_swap_sessions_root: None,
        }
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn now() -> DateTime<Utc> {
    Utc::now()
}
fn stamp(hours_ago: i64) -> String {
    (now() - chrono::Duration::hours(hours_ago)).to_rfc3339()
}
fn claude_line(ts: &str, id: &str, tokens: i64) -> String {
    format!("{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"sessionId\":\"{id}\",\"message\":{{\"model\":\"synthetic\",\"usage\":{{\"input_tokens\":{tokens},\"output_tokens\":0}}}}}}\n")
}
fn codex_line(ts: &str, tokens: i64) -> String {
    format!("{{\"type\":\"event_msg\",\"timestamp\":\"{ts}\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":{tokens},\"cached_input_tokens\":0,\"output_tokens\":0}}}}}}}}\n")
}
fn write(path: &Path, data: &str) {
    fs::write(path, data).unwrap();
}
#[tokio::test]
async fn backfill_then_complete_live_lines_without_replay() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let c = opts.claude_projects_root.join("one.jsonl");
    let x = opts.codex_sessions_root.join("rollout.jsonl");
    write(
        &c,
        &(claude_line(&stamp(48), "old", 100) + &claude_line(&stamp(0), "today", 7)),
    );
    write(&x, &codex_line(&stamp(0), 5));
    let reader = Reader::new(opts, now()).unwrap();
    let stop = CancellationToken::new();
    let first = reader.sample(now(), &stop).await.unwrap();
    assert_eq!(first.claude.today_total_tokens, 7);
    assert_eq!(first.codex.today_total_tokens, 5);
    assert_eq!(first.claude.activity_sources, vec![ActivitySource::Jsonl]);
    let same = reader.clone().sample(now(), &stop).await.unwrap();
    assert_eq!(same.claude.today_total_tokens, 7);
    let mut f = fs::OpenOptions::new().append(true).open(&c).unwrap();
    let line = claude_line(&stamp(0), "today", 11);
    let cut = line.len() / 2;
    f.write_all(&line.as_bytes()[..cut]).unwrap();
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        7
    );
    f.write_all(&line.as_bytes()[cut..]).unwrap();
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        18
    );
    let new = opts_path(&tmp, "claude/new.jsonl");
    write(&new, &claude_line(&stamp(0), "new", 3));
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        21
    );
}
fn opts_path(tmp: &Temp, suffix: &str) -> PathBuf {
    tmp.0.join(suffix)
}
#[tokio::test]
async fn replaced_inode_and_oversized_line() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let path = opts.claude_projects_root.join("one.jsonl");
    let reader = Reader::new(opts, now()).unwrap();
    let stop = CancellationToken::new();
    reader.sample(now(), &stop).await.unwrap();
    write(&path, &claude_line(&stamp(0), "a", 2));
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        2
    );
    let replacement = path.with_extension("tmp");
    write(&replacement, &claude_line(&stamp(0), "b", 3));
    fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        5
    );
    let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&vec![b'x'; activity::MAX_LINE_BYTES + 1])
        .unwrap();
    f.write_all(b"\n").unwrap();
    f.write_all(claude_line(&stamp(0), "c", 4).as_bytes())
        .unwrap();
    let mut got = reader.sample(now(), &stop).await.unwrap();
    for _ in 0..3 {
        if got.claude.today_total_tokens == 9 {
            break;
        }
        got = reader.sample(now(), &stop).await.unwrap();
    }
    assert_eq!(got.claude.today_total_tokens, 9);
}
#[tokio::test]
async fn rejects_links_and_reports_missing_roots() {
    use std::os::unix::fs::symlink;
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let original = opts.claude_projects_root.join("real.jsonl");
    write(&original, &claude_line(&stamp(0), "a", 2));
    symlink(&original, opts.claude_projects_root.join("link.jsonl")).unwrap();
    fs::hard_link(&original, opts.claude_projects_root.join("hard.jsonl")).unwrap();
    let reader = Reader::new(opts, now()).unwrap();
    let got = reader
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(got.claude.today_total_tokens, 0);
    assert_eq!(got.claude_diagnostics.issue, Some(Issue::Unsafe));
    let missing = Reader::new(
        Options {
            claude_projects_root: tmp.0.join("absent"),
            codex_sessions_root: tmp.0.join("also-absent"),
            claude_swap_sessions_root: None,
        },
        now(),
    )
    .unwrap();
    let got = missing
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(got.claude_diagnostics.issue, Some(Issue::Missing));
}
#[tokio::test]
async fn extreme_clock_empty_roots_is_safe() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let reader = Reader::new(opts, DateTime::<Utc>::MIN_UTC).unwrap();
    let got = reader
        .sample(DateTime::<Utc>::MIN_UTC, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(got.claude.today_total_tokens, 0);
}

#[tokio::test]
async fn swap_roots_are_discovered_and_bound_to_account_number() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let mut opts = tmp.roots();
    let sessions = tmp.0.join("swap/sessions");
    let projects = sessions.join("02-synthetic-account/projects");
    fs::create_dir_all(&projects).unwrap();
    fs::create_dir_all(sessions.join("bad-name/projects")).unwrap();
    write(
        &projects.join("session.jsonl"),
        &claude_line(&stamp(0), "swap", 6),
    );
    opts.claude_swap_sessions_root = Some(sessions);
    let reader = Reader::new(opts, now()).unwrap();
    let mut budget = Budget {
        bytes: MAX_BOOTSTRAP_BYTES,
        files: 0,
        dirs: 0,
        entries: 0,
        deadline: Instant::now() + SCAN_DEADLINE,
    };
    let roots = reader
        .inner
        .lock()
        .unwrap()
        .discover(
            &mut budget,
            &CancellationToken::new(),
            &mut Diagnostics::default(),
        )
        .unwrap();
    assert_eq!(roots.iter().filter(|r| r.account == 2).count(), 1);
    assert_eq!(
        reader
            .sample(now(), &CancellationToken::new())
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        6
    );
}

#[tokio::test]
async fn incomplete_reconcile_keeps_existing_offsets() {
    use std::os::unix::fs::symlink;
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let root = opts.claude_projects_root.clone();
    let path = root.join("one.jsonl");
    write(&path, &claude_line(&stamp(0), "one", 2));
    let reader = Reader::new(opts, now()).unwrap();
    reader
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    fs::remove_file(&path).unwrap();
    symlink(tmp.0.join("missing"), root.join("untrusted.jsonl")).unwrap();
    reader
        .inner
        .lock()
        .unwrap()
        .roots
        .get_mut(&root)
        .unwrap()
        .reconciled = None;
    let got = reader
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    assert!(got.claude_diagnostics.incomplete);
    assert!(reader
        .inner
        .lock()
        .unwrap()
        .roots
        .get(&root)
        .unwrap()
        .offsets
        .contains_key(&path));
}

#[tokio::test]
async fn startup_partial_line_completes_once_after_append() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let path = opts.claude_projects_root.join("partial.jsonl");
    let first = claude_line(&stamp(0), "complete", 2);
    let next = claude_line(&stamp(0), "partial", 7);
    let split = next.len() / 2;
    write(&path, &(first.clone() + &next[..split]));
    let reader = Reader::new(opts, now()).unwrap();
    let stop = CancellationToken::new();
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        2
    );
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        2
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&next.as_bytes()[split..])
        .unwrap();
    assert_eq!(
        reader
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        9
    );
    assert_eq!(
        reader
            .clone()
            .sample(now(), &stop)
            .await
            .unwrap()
            .claude
            .today_total_tokens,
        9
    );
}

#[tokio::test]
async fn retained_swap_roots_are_globally_bounded_without_replay() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let mut opts = tmp.roots();
    let sessions = tmp.0.join("swap/sessions");
    let retired = tmp.0.join("retired");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&retired).unwrap();
    opts.claude_swap_sessions_root = Some(sessions.clone());
    let reader = Reader::new(opts, now()).unwrap();
    let stop = CancellationToken::new();
    reader.sample(now(), &stop).await.unwrap();
    for account in 1..=MAX_ROOTS - 2 {
        let name = format!("{account}-synthetic");
        let active = sessions.join(&name);
        let project = active.join("projects");
        fs::create_dir_all(&project).unwrap();
        if account == 1 {
            write(
                &project.join("one.jsonl"),
                &claude_line(&stamp(0), "one", 3),
            );
        }
        let got = reader.sample(now(), &stop).await.unwrap();
        if account == 1 {
            assert_eq!(got.claude.today_total_tokens, 3);
        }
        fs::rename(active, retired.join(name)).unwrap();
    }
    assert_eq!(reader.inner.lock().unwrap().roots.len(), MAX_ROOTS);
    let overflow = sessions.join("999-synthetic");
    fs::create_dir_all(overflow.join("projects")).unwrap();
    let got = reader.sample(now(), &stop).await.unwrap();
    assert_eq!(got.claude_diagnostics.issue, Some(Issue::Limit));
    assert_eq!(reader.inner.lock().unwrap().roots.len(), MAX_ROOTS);
    fs::rename(overflow, retired.join("999-synthetic")).unwrap();
    fs::rename(retired.join("1-synthetic"), sessions.join("1-synthetic")).unwrap();
    let got = reader.sample(now(), &stop).await.unwrap();
    assert_eq!(got.claude.today_total_tokens, 3);
    assert_eq!(reader.inner.lock().unwrap().roots.len(), MAX_ROOTS);
}

#[tokio::test]
async fn cancelled_call_keeps_saved_offset() {
    let _serial = test_lock().await;
    let tmp = Temp::new();
    let opts = tmp.roots();
    let root = opts.claude_projects_root.clone();
    let path = root.join("one.jsonl");
    write(&path, &claude_line(&stamp(0), "one", 2));
    let reader = Reader::new(opts, now()).unwrap();
    reader
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    let before = reader
        .inner
        .lock()
        .unwrap()
        .roots
        .get(&root)
        .unwrap()
        .offsets[&path]
        .pos;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        reader.sample(now(), &cancelled).await.unwrap_err(),
        Error::Cancelled
    );
    let after = reader
        .inner
        .lock()
        .unwrap()
        .roots
        .get(&root)
        .unwrap()
        .offsets[&path]
        .pos;
    assert_eq!(before, after);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(claude_line(&stamp(0), "two", 3).as_bytes())
        .unwrap();
    let got = reader
        .sample(now(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(got.claude.today_total_tokens, 5);
}
