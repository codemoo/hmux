use super::browser_terminal::Home;
use super::*;
use hmux_gateway::hub::Hub;
use hmux_protocol::protobuf::types as p;
use std::ffi::OsStr;

fn workspace(change: Value) -> Value {
    json!({"operation":"workspace","payload":{"change":change}})
}
fn change(n: u64) -> Value {
    json!({"operation_id":format!("operation-test-{n:04}"),"revision":0,"base":[],"tabs":[{"id":format!("${n}"),"created_at":100+n}]})
}
async fn catalog(home: &mut Home, hub: &Hub) {
    home.send(p::envelope::Body::Catalog(Box::new(
        hmux_protocol::snapshots::catalog_from_json(
            br#"{"sessions":[{"id":"$1","created_at":101},{"id":"$2","created_at":102}]}"#,
        )
        .unwrap(),
    )))
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while !hub.snapshot().online {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn workspace_accounts_merge_restart_and_preserve_primary_home_routing() {
    let fixture = Fixture::new();
    let mut other: Value = serde_json::from_str(
        &fs::read_to_string(fixture.root.join("credentials.json.users/guest.json")).unwrap(),
    )
    .unwrap();
    other["username"] = json!("other");
    write_private(
        &fixture.root.join("credentials.json.users/other.json"),
        other.to_string(),
    );
    let (hub, _events) = Hub::new();
    let server = Arc::new(
        fixture
            .start_with_options(Some(hub.clone()), false, true)
            .await,
    );
    let guest = server.login("guest").await;
    let primary = server.login("primary").await;
    let other = server.login("other").await;
    let csrf = server.session(&guest).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let headers = [
        ("Cookie", guest.as_str()),
        ("Origin", "https://hmux.example"),
        ("X-CSRF-Token", csrf.as_str()),
    ];
    assert_eq!(
        server
            .request(
                "POST",
                "/api/action",
                &headers[..2],
                Some(workspace(change(1)))
            )
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request("POST", "/api/action", &headers, Some(workspace(change(1))))
            .await
            .code,
        502
    );
    let mut home = Home::new(&hub).await;
    catalog(&mut home, &hub).await;
    for invalid in [
        json!({"operation":"kill"}),
        json!({"operation":"workspace","payload":[]}),
        json!({"operation":"workspace","payload":{"profile":"primary"}}),
    ] {
        assert_eq!(
            server
                .request("POST", "/api/action", &headers, Some(invalid))
                .await
                .code,
            400
        );
    }
    assert_eq!(
        server
            .request("GET", "/api/action", &headers, None)
            .await
            .code,
        405
    );
    let saved = server
        .request("POST", "/api/action", &headers, Some(workspace(change(1))))
        .await;
    assert_eq!(saved.code, 200);
    assert_eq!(saved.json()["revision"], 1);
    assert_eq!(saved.json()["tabs"][0]["id"], "$1");
    let replay = server
        .request("POST", "/api/action", &headers, Some(workspace(change(1))))
        .await;
    assert_eq!(replay.json(), saved.json());
    let added = server
        .request("POST", "/api/action", &headers, Some(workspace(change(2))))
        .await
        .json();
    assert_eq!(added["revision"], 2);
    assert_eq!(added["tabs"].as_array().unwrap().len(), 2);
    let mut future = change(1);
    future["operation_id"] = json!("operation-future1");
    future["revision"] = json!(99);
    assert_eq!(
        server
            .request("POST", "/api/action", &headers, Some(workspace(future)))
            .await
            .json()["conflict"],
        "workspace_conflict"
    );
    let other_csrf = server.session(&other).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_headers = [
        ("Cookie", other.as_str()),
        ("Origin", "https://hmux.example"),
        ("X-CSRF-Token", other_csrf.as_str()),
    ];
    assert_eq!(
        server
            .request(
                "POST",
                "/api/action",
                &other_headers,
                Some(workspace(Value::Null))
            )
            .await
            .json()["tabs"],
        json!([])
    );
    // Primary uses the Home workspace, not a gateway account directory.
    let primary_csrf = server.session(&primary).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let pending = tokio::spawn({
        let server = server.clone();
        async move {
            server
                .request(
                    "POST",
                    "/api/action",
                    &[
                        ("Cookie", primary.as_str()),
                        ("Origin", "https://hmux.example"),
                        ("X-CSRF-Token", primary_csrf.as_str()),
                    ],
                    Some(workspace(Value::Null)),
                )
                .await
        }
    });
    let p::envelope::Body::Request(request) = home.receive().await else {
        panic!("expected primary Home request")
    };
    assert_eq!(request.operation, p::Operation::Workspace as i32);
    home.send(p::envelope::Body::Response(p::Response {
        id: request.id,
        error: String::new(),
        result: Some(p::response::Result::Workspace(Box::new(
            p::WorkspaceSnapshot {
                version: 1,
                initialized: true,
                ..Default::default()
            },
        ))),
    }))
    .await;
    assert_eq!(pending.await.unwrap().json()["version"], 1);
    let path = fixture
        .root
        .join("web-profiles")
        .join(auth::account_profile("guest"))
        .join("shared-workspace/workspace.json");
    let persisted: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(persisted, added);
    home.stop().await;
    Arc::try_unwrap(server).ok().unwrap().stop().await;
    let (hub, _events) = Hub::new();
    let server = fixture
        .start_with_options(Some(hub.clone()), false, true)
        .await;
    let mut home = Home::new(&hub).await;
    catalog(&mut home, &hub).await;
    assert_eq!(
        server
            .request(
                "POST",
                "/api/action",
                &headers,
                Some(workspace(Value::Null))
            )
            .await
            .json(),
        added
    );
    home.stop().await;
    server.stop().await;
}

#[tokio::test]
async fn forwarded_actions_sanitize_fields_and_cancel_on_logout() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = Arc::new(fixture.start_with_home(Some(hub.clone())).await);
    let cookie = server.login("primary").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut home = Home::new(&hub).await;
    let launch = |body: Value| {
        let server = server.clone();
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        tokio::spawn(async move {
            server
                .request(
                    "POST",
                    "/api/action",
                    &[
                        ("Cookie", cookie.as_str()),
                        ("Origin", "https://hmux.example"),
                        ("X-CSRF-Token", csrf.as_str()),
                    ],
                    Some(body),
                )
                .await
        })
    };
    let request = launch(
        json!({"type":"close","id":"forged-view-id","operation":"conversation","session":{"id":"$7","created_at":77},"payload":{"cursor":"test"},"data":"eA==","cols":80,"rows":25,"error":"forged","received":1,"capabilities":["forged"]}),
    );
    let p::envelope::Body::Request(sent) = home.receive().await else {
        panic!("expected request")
    };
    assert_ne!(sent.id, "forged-view-id");
    assert_eq!(sent.operation, p::Operation::Conversation as i32);
    assert_eq!(sent.session.unwrap().created_at, 77);
    assert!(matches!(sent.payload, Some(p::request::Payload::Empty(_))));
    home.send(p::envelope::Body::Response(p::Response {
        id: sent.id,
        error: String::new(),
        result: Some(p::response::Result::Conversation(Box::new(
            p::ConversationResult {
                provider: "codex".into(),
                session_id: "$7".into(),
                created_at: 77,
                status: "ready".into(),
                messages: Some(p::ConversationMessages { items: Vec::new() }),
                truncated: false,
            },
        ))),
    }))
    .await;
    let conversation = request.await.unwrap().json();
    assert_eq!(conversation["session_id"], "$7");
    assert_eq!(conversation["messages"], json!([]));
    let profiles = launch(json!({"operation":"profiles"}));
    let p::envelope::Body::Request(sent) = home.receive().await else {
        panic!("expected sessionless profiles request")
    };
    assert_eq!(sent.operation, p::Operation::Profiles as i32);
    assert!(sent.session.is_none());
    home.send(p::envelope::Body::Response(p::Response {
        id: sent.id,
        error: String::new(),
        result: Some(p::response::Result::Profiles(p::ProfilesResult {
            items: vec![p::Profile {
                id: "shell".into(),
                label: "Shell".into(),
            }],
        })),
    }))
    .await;
    assert_eq!(
        profiles.await.unwrap().json(),
        json!([{"id":"shell","label":"Shell"}])
    );
    let timed = launch(json!({"operation":"profiles"}));
    let p::envelope::Body::Request(sent) = home.receive().await else {
        panic!("expected request")
    };
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(20)).await;
    let timed = timed.await.unwrap();
    tokio::time::resume();
    assert_eq!(timed.code, 502);
    let p::envelope::Body::Cancel(cancel) = home.receive().await else {
        panic!("expected timed-out request cancellation")
    };
    assert_eq!(cancel.id, sent.id);
    let request = launch(json!({"operation":"providers"}));
    let p::envelope::Body::Request(sent) = home.receive().await else {
        panic!("expected request")
    };
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[
                    ("Cookie", cookie.as_str()),
                    ("Origin", "https://hmux.example"),
                    ("X-CSRF-Token", csrf.as_str())
                ],
                None
            )
            .await
            .code,
        200
    );
    assert_eq!(request.await.unwrap().code, 401);
    let p::envelope::Body::Cancel(cancel) = home.receive().await else {
        panic!("expected Home cancellation")
    };
    assert_eq!(cancel.id, sent.id);
    home.stop().await;
    Arc::try_unwrap(server).ok().unwrap().stop().await;
}

#[tokio::test]
async fn revoked_workspace_waiter_does_not_write_after_lock_release() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = Arc::new(
        fixture
            .start_with_options(Some(hub.clone()), false, true)
            .await,
    );
    let cookie = server.login("guest").await;
    let csrf = server.session(&cookie).await.json()["csrf"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut home = Home::new(&hub).await;
    catalog(&mut home, &hub).await;
    let root = hmux_core::PrivateDir::open(&fixture.root.join("web-profiles"))
        .unwrap()
        .create_private_child(OsStr::new(&auth::account_profile("guest")))
        .unwrap()
        .create_private_child(OsStr::new("shared-workspace"))
        .unwrap();
    let held = root.try_lock(OsStr::new("lock")).unwrap().unwrap();
    let pending = tokio::spawn({
        let server = server.clone();
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        async move {
            server
                .request(
                    "POST",
                    "/api/action",
                    &[
                        ("Cookie", cookie.as_str()),
                        ("Origin", "https://hmux.example"),
                        ("X-CSRF-Token", csrf.as_str()),
                    ],
                    Some(workspace(change(1))),
                )
                .await
        }
    });
    // Give the request a chance to enter the locked transaction; the lower-level
    // store test independently uses a deterministic admission barrier.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        server
            .request(
                "POST",
                "/api/logout",
                &[
                    ("Cookie", cookie.as_str()),
                    ("Origin", "https://hmux.example"),
                    ("X-CSRF-Token", csrf.as_str())
                ],
                None
            )
            .await
            .code,
        200
    );
    assert_eq!(pending.await.unwrap().code, 401);
    drop(held);
    home.stop().await;
    Arc::try_unwrap(server).ok().unwrap().stop().await;
    assert_eq!(
        root.read_private(OsStr::new("workspace.json"), 32768)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
}
