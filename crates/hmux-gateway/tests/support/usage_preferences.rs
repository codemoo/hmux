use super::browser_terminal::Home;
use super::*;
use hmux_gateway::{hub::Hub, usage_preferences::Preferences};
use hmux_protocol::{protobuf::types as p, snapshots};

#[tokio::test]
async fn preferences_http_authorization_conflict_restart_and_state_account_scope() {
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_services(Some(hub.clone()), true).await;
    assert_eq!(
        server
            .request("GET", "/api/account/usage", &[], None)
            .await
            .code,
        401
    );
    let primary = server.login("primary").await;
    let guest = server.login("guest").await;
    let session = server.session(&primary).await.json();
    let csrf = session["csrf"].as_str().unwrap();
    let mut next = serde_json::to_value(Preferences::default()).unwrap();
    next["claude"]["enabled"] = json!(false);
    next["codex"]["source"] = json!("cli");
    let auth = [
        ("Cookie", primary.as_str()),
        ("Origin", "https://hmux.example"),
        ("X-CSRF-Token", csrf),
    ];
    let positional =
        json!([1,0,{"enabled":false,"source":"cswap"},{"enabled":true,"source":"cli"}]);
    assert_eq!(
        server
            .request("POST", "/api/account/usage", &auth, Some(positional))
            .await
            .code,
        400
    );
    assert_eq!(
        server
            .request("POST", "/api/account/usage", &auth[..2], Some(next.clone()))
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/account/usage",
                &[
                    ("Cookie", &primary),
                    ("Origin", "https://other.example"),
                    ("X-CSRF-Token", csrf)
                ],
                Some(next.clone())
            )
            .await
            .code,
        403
    );
    let mut invalid = next.clone();
    invalid["codex"]["source"] = json!("cswap");
    assert_eq!(
        server
            .request("POST", "/api/account/usage", &auth, Some(invalid))
            .await
            .code,
        400
    );
    let saved = server
        .request("POST", "/api/account/usage", &auth, Some(next.clone()))
        .await;
    assert_eq!(saved.code, 200);
    let saved = saved.json();
    assert_eq!(saved["revision"], 1);
    assert_eq!(
        server
            .request("POST", "/api/account/usage", &auth, Some(next))
            .await
            .code,
        409
    );
    let offline = server
        .request("GET", "/api/state", &auth[..1], None)
        .await
        .json();
    assert_eq!(
        offline,
        json!({"online":false,"catalog":null,"usage":{},"usage_preferences":saved})
    );
    let other = server
        .request("GET", "/api/state", &[("Cookie", &guest)], None)
        .await
        .json();
    assert_eq!(
        other["usage_preferences"],
        serde_json::to_value(Preferences::default()).unwrap()
    );
    let mut home = Home::new(&hub).await;
    home.send(p::envelope::Body::Catalog(Box::new(
        snapshots::catalog_from_json(br#"{"sessions":[{"id":"$1","created_at":42}]}"#).unwrap(),
    )))
    .await;
    home.send(p::envelope::Body::Usage(Box::new(
        snapshots::usage_to_proto(hmux_usage::Snapshot::degraded(
            hmux_usage::Provider::Codex,
            1,
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "ok",
        ))
        .unwrap(),
    )))
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        while hub.snapshot().codex_usage.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let online = server
        .request("GET", "/api/state", &auth[..1], None)
        .await
        .json();
    assert_eq!(online["online"], true);
    assert_eq!(online["catalog"]["sessions"][0]["created_at"], 42);
    assert_eq!(online["usage"]["codex"]["schema"], 1);
    assert_eq!(online["usage"]["codex"]["provider"], "codex");
    assert_eq!(online["usage"]["codex"]["status"]["state"], "ok");
    assert!(online["usage"].get("claude").is_none());
    assert_eq!(online["usage_preferences"], saved);
    // A valid catalog tree and bounded public account rows together exceed
    // the single-message budget, while each typed snapshot remains admissible.
    let sessions: Vec<_> = (1..=1020)
        .map(|id| json!({"id":format!("${id}"),"created_at":id,"name":"x".repeat(3900)}))
        .collect();
    let large_catalog = serde_json::to_vec(&json!({"sessions":sessions})).unwrap();
    home.send(p::envelope::Body::Catalog(Box::new(
        snapshots::catalog_from_json(&large_catalog).unwrap(),
    )))
    .await;
    for provider in [hmux_usage::Provider::Claude, hmux_usage::Provider::Codex] {
        let mut snapshot = hmux_usage::Snapshot::degraded(
            provider,
            2,
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "ok",
        );
        snapshot.accounts = (1..=128)
            .map(|number| hmux_usage::Account {
                number,
                display_name: "x".repeat(256),
                email: if provider == hmux_usage::Provider::Claude {
                    "x".repeat(256)
                } else {
                    String::new()
                },
                ..Default::default()
            })
            .collect();
        home.send(p::envelope::Body::Usage(Box::new(
            snapshots::usage_to_proto(snapshot).unwrap(),
        )))
        .await;
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while hub
            .snapshot()
            .codex_usage
            .as_ref()
            .is_none_or(|v| v.len() < 30_000)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let large = server.request("GET", "/api/state", &auth[..1], None).await;
    assert_eq!(large.code, 200);
    assert!(large.body.len() > (4 << 20));
    let large = large.json();
    assert_eq!(large["catalog"]["sessions"].as_array().unwrap().len(), 1020);
    assert_eq!(large["catalog"]["sessions"][0]["name"], "x".repeat(3900));
    for provider in ["claude", "codex"] {
        assert_eq!(large["usage"][provider]["provider"], provider);
        assert_eq!(large["usage"][provider]["schema"], 1);
        assert_eq!(
            large["usage"][provider]["accounts"]
                .as_array()
                .unwrap()
                .len(),
            128
        );
    }
    assert_eq!(large["usage_preferences"], saved);
    home.stop().await;
    let cleared = server
        .request("GET", "/api/state", &auth[..1], None)
        .await
        .json();
    assert_eq!(cleared["online"], false);
    assert_eq!(cleared["catalog"], Value::Null);
    assert_eq!(cleared["usage"], json!({}));
    server.stop().await;
    let server = fixture.start_with_services(Some(hub), true).await;
    let after = server
        .request("GET", "/api/account/usage", &auth[..1], None)
        .await;
    assert_eq!(after.code, 200);
    assert_eq!(after.json(), saved);
    server.stop().await;
}

#[tokio::test]
async fn corrupt_settings_are_omitted_from_state_and_revoked_body_cannot_write() {
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let (hub, _events) = Hub::new();
    let server = fixture.start_with_services(Some(hub), true).await;
    let cookie = server.login("primary").await;
    let session = server.session(&cookie).await.json();
    let csrf = session["csrf"].as_str().unwrap();
    let key = format!(
        "{:x}",
        Sha256::digest(format!(
            "{}\0{}",
            session["username"].as_str().unwrap(),
            session["profile"].as_str().unwrap()
        ))
    );
    let path = fixture
        .root
        .join("credentials.json.usage-preferences")
        .join(format!("{key}.json"));
    write_private(&path, "{}".into());
    let headers = [
        ("Cookie", cookie.as_str()),
        ("Origin", "https://hmux.example"),
        ("X-CSRF-Token", csrf),
    ];
    assert_eq!(
        server
            .request("GET", "/api/account/usage", &headers[..1], None)
            .await
            .code,
        503
    );
    let state = server
        .request("GET", "/api/state", &headers[..1], None)
        .await;
    assert_eq!(state.code, 200);
    assert!(state.json().get("usage_preferences").is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
    fs::remove_file(&path).unwrap();
    let raw = serde_json::to_vec(&Preferences::default()).unwrap();
    let mut pending = TcpStream::connect(server.address).await.unwrap();
    let head=format!("POST /api/account/usage HTTP/1.1\r\nHost: hmux.example\r\nOrigin: https://hmux.example\r\nCookie: {cookie}\r\nX-CSRF-Token: {csrf}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",raw.len());
    pending.write_all(head.as_bytes()).await.unwrap();
    pending.write_all(&raw[..1]).await.unwrap();
    assert_eq!(
        server
            .request("POST", "/api/logout", &headers, None)
            .await
            .code,
        200
    );
    pending.write_all(&raw[1..]).await.unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), pending.read_to_end(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 401"));
    assert!(!path.exists());
    server.stop().await;
}
