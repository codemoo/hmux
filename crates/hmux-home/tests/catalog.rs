use hmux_core::command::CommandRunner;
use hmux_home::catalog::{parse_basic_catalog, TmuxCatalogReader, TmuxSocket};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

const SEP: &str = "|:hmux-sep-v1:|";

#[test]
fn matches_existing_go_basic_catalog_oracle() {
    let fixtures: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/catalog-v1/go-oracle.json"
    ))
    .unwrap();
    for case in fixtures.as_array().unwrap() {
        let result = parse(
            case["sessions"].as_str().unwrap(),
            case["windows"].as_str().unwrap(),
        );
        assert_eq!(
            result.is_ok(),
            case["valid"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if let Ok(catalog) = result {
            assert_eq!(
                serde_json::to_value(catalog).unwrap(),
                case["catalog"],
                "{}",
                case["name"]
            );
        }
    }
}
fn row(fields: [&str; 8]) -> String {
    fields.join(SEP) + "\n"
}
fn session(id: &str, marker: &str, group: &str) -> String {
    row([
        id,
        "real\u{1b}[31m",
        "1700000000",
        "1700000200",
        "0",
        "2",
        marker,
        group,
    ])
}
fn window(id: &str, active: &str, width: &str) -> String {
    row([
        id,
        "editor",
        active,
        "/tmp/긴 경로",
        "zsh",
        width,
        "40",
        "123",
    ])
}
fn parse(
    sessions: &str,
    windows: &str,
) -> Result<hmux_model::Catalog, hmux_home::catalog::CatalogError> {
    parse_basic_catalog(
        sessions.as_bytes(),
        windows.as_bytes(),
        "2026-09-24T00:00:00Z".into(),
    )
}

#[test]
fn parses_visible_session_and_grouped_attachment() {
    let mut sessions = session("$7", "", "2");
    sessions.push_str(&session("$8", "1", "2"));
    let mut windows = window("$7", "1", "120");
    windows.push_str(&window("$7", "0", "80"));
    windows.push_str(&window("$8", "1", "80"));
    let catalog = parse(&sessions, &windows).unwrap();
    assert_eq!(catalog.protocol_version, 1);
    let sessions = catalog.sessions.unwrap();
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(
        (&s.id, s.created_at, s.attached),
        (&"$7".into(), 1_700_000_000, 2)
    );
    assert_eq!((s.width, s.height, s.pane_pid), (120, 40, 123));
    assert_eq!(s.current_path, "/tmp/긴 경로");
    assert_eq!(s.window_names.as_ref().unwrap(), &vec!["editor", "editor"]);
    assert_eq!(
        (&s.kind[..], &s.runtime[..], &s.state[..], &s.process[..]),
        ("shell", "process", "running", "zsh")
    );
    assert!(!s.name.contains('\u{1b}'));
}

#[test]
fn sorts_by_activity_then_id_and_keeps_empty_array() {
    let mut sessions = session("$9", "", "");
    sessions.push_str(&session("$3", "", ""));
    let catalog = parse(&sessions, "").unwrap();
    let sessions = catalog.sessions.unwrap();
    assert_eq!(
        sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["$3", "$9"]
    );
    assert!(sessions.iter().all(|s| s.window_names.is_none()));
    assert_eq!(
        serde_json::to_value(&sessions[0]).unwrap()["window_names"],
        serde_json::Value::Null
    );
    let empty = parse("", "").unwrap();
    assert_eq!(
        serde_json::to_value(empty).unwrap()["sessions"],
        serde_json::json!([])
    );
}

#[test]
fn row_count_limits_include_exact_boundary() {
    let mut sessions = String::new();
    for id in 0..10_000 {
        sessions.push_str(&session(&format!("${id}"), "", ""));
    }
    assert_eq!(
        parse(&sessions, "").unwrap().sessions.unwrap().len(),
        10_000
    );
    sessions.push_str(&session("$10000", "", ""));
    assert!(parse(&sessions, "").is_err());

    let one_window = window("$99", "0", "80"); // Unknown session is still a counted row.
    let mut windows = one_window.repeat(100_000);
    assert!(parse("", &windows).is_ok());
    windows.push_str(&one_window);
    assert!(parse("", &windows).is_err());
}

#[test]
fn separator_collision_and_newline_semantics_match_tmux_parser() {
    let collision = row(["$7", &format!("name{SEP}extra"), "1", "1", "0", "1", "", ""]);
    assert!(parse(&collision, "").is_err());
    let collision = row([
        "$7",
        &format!("name{SEP}extra"),
        "0",
        "",
        "",
        "80",
        "24",
        "",
    ]);
    assert!(parse(&session("$7", "", ""), &collision).is_err());
    assert!(parse(&(session("$7", "", "") + "\n"), "").is_err());
    assert!(parse("\n", "").unwrap().sessions.unwrap().is_empty());
}

#[test]
fn rejects_bad_identity_numbers_dimensions_and_rows() {
    for bad in [
        session("$7;bad", "", ""),
        session("$7", "", "invalid"),
        row(["$7", "real", "0", "0", "10001", "1", "", ""]),
        row(["$7", "real", "-1", "0", "0", "1", "", ""]),
        "malformed\n".into(),
    ] {
        assert!(parse(&bad, "").is_err(), "accepted {bad:?}");
    }
    let duplicate = session("$7", "", "") + &session("$7", "", "");
    assert!(parse(&duplicate, "").is_err());
    for bad in [
        window("$7", "1", "100001"),
        row(["$7", "x", "1", "", "", "80", "24", "0"]),
    ] {
        assert!(parse(&session("$7", "", ""), &bad).is_err());
    }
    assert!(parse(&session("$7", "", ""), "malformed\n").is_err());
    assert!(parse_basic_catalog(&[0xff], &[], "2026-09-24T00:00:00Z".into()).is_err());
    assert!(parse_basic_catalog(
        &vec![b'x'; 16 * 1024 * 1024 + 1],
        &[],
        "2026-09-24T00:00:00Z".into()
    )
    .is_err());
}

#[test]
fn unavailable_dimensions_are_zero_and_inactive_bad_dimensions_are_ignored() {
    let active = row(["$7", "editor", "1", "/tmp", "zsh", "", "", ""]);
    let inactive = row([
        "$7", "logs", "0", "/tmp", "tail", "invalid", "invalid", "invalid",
    ]);
    let catalog = parse(&session("$7", "", ""), &(active + &inactive)).unwrap();
    let s = &catalog.sessions.unwrap()[0];
    assert_eq!((s.width, s.height, s.pane_pid), (0, 0, 0));
    assert_eq!(s.window_names.as_ref().unwrap(), &["editor", "logs"]);
}

fn script(body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hmux-catalog-test-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fake-tmux");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[tokio::test]
async fn adapter_uses_only_two_bounded_read_commands_and_socket_option() {
    let path = script(&format!(
        r#"
[ "$1" = "-L" ] && [ "$2" = "hmux-test" ] || exit 8
shift 2
case "$1" in
  list-sessions) [ "$2" = "-F" ] || exit 9; case "$3" in *session_group_attached*) ;; *) exit 12 ;; esac; printf '%s\n' '{}' ;;
  list-windows) [ "$2" = "-a" ] && [ "$3" = "-F" ] || exit 10; case "$4" in *window_width*) ;; *) exit 13 ;; esac; printf '%s\n' '{}' ;;
  *) exit 11 ;;
esac
"#,
        session("$7", "", "1").trim_end(),
        window("$7", "1", "80").trim_end()
    ));
    let reader = TmuxCatalogReader::new(
        path.clone(),
        Some(TmuxSocket::Name("hmux-test".into())),
        Duration::from_secs(2),
    )
    .unwrap();
    let catalog = reader
        .read_basic(&CommandRunner::new(1).unwrap())
        .await
        .unwrap();
    assert_eq!(catalog.sessions.unwrap()[0].attached, 1);
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[tokio::test]
async fn only_missing_server_becomes_empty_catalog() {
    let path =
        script("printf '%s\\n' 'error connecting to fake (No such file or directory)' >&2\nexit 1");
    let reader = TmuxCatalogReader::new(path.clone(), None, Duration::from_secs(2)).unwrap();
    let catalog = reader
        .read_basic(&CommandRunner::new(1).unwrap())
        .await
        .unwrap();
    assert!(catalog.sessions.unwrap().is_empty());
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
    let path = script("printf '%s\\n' 'permission denied' >&2\nexit 1");
    let reader = TmuxCatalogReader::new(path.clone(), None, Duration::from_secs(2)).unwrap();
    assert!(reader
        .read_basic(&CommandRunner::new(1).unwrap())
        .await
        .is_err());
    fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn rejects_unsafe_socket_options() {
    assert!(TmuxCatalogReader::new(
        PathBuf::from("tmux"),
        Some(TmuxSocket::Name("../other".into())),
        Duration::from_secs(2)
    )
    .is_err());
    assert!(TmuxCatalogReader::new(
        PathBuf::from("tmux"),
        Some(TmuxSocket::Path(PathBuf::from("relative"))),
        Duration::from_secs(2)
    )
    .is_err());
    assert!(TmuxCatalogReader::new(PathBuf::from("tmux"), None, Duration::ZERO).is_err());
}

#[test]
fn executable_validation_precedes_any_catalog_or_view_work() {
    for executable in [
        PathBuf::from("tmux"),
        PathBuf::from("relative/tmux"),
        PathBuf::from("/tmp/nul\0tmux"),
        PathBuf::from(format!("/{}", "a".repeat(1024))),
    ] {
        assert!(TmuxCatalogReader::new(executable, None, Duration::from_secs(3)).is_err());
    }
}
