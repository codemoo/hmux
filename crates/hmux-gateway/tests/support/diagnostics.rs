use super::*;
fn batch(sequence: i64) -> Value {
    json!({"version":1,"client":"12345678-1234-4234-8234-123456789abc","build":"app-fixture.js","events":[{"sequence":sequence,"at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64,"kind":"terminal-failed","reason":"network","online":true,"visible":true}]})
}

#[tokio::test]
async fn diagnostics_http_privacy_rate_authentication_restart_and_revoked_body() {
    let fixture = Fixture::new();
    let server = fixture.start_all(None, false, false, true).await;
    assert_eq!(
        server
            .request(
                "POST",
                "/api/diagnostics",
                &[("Origin", "https://hmux.example")],
                Some(batch(1))
            )
            .await
            .code,
        401
    );
    let primary = server.login("primary").await;
    let guest = server.login("guest").await;
    let access = server.session(&primary).await.json();
    let csrf = access["csrf"].as_str().unwrap();
    let auth = [
        ("Cookie", primary.as_str()),
        ("Origin", "https://hmux.example"),
        ("X-CSRF-Token", csrf),
    ];
    let empty = server
        .request("GET", "/api/diagnostics", &auth[..1], None)
        .await
        .json();
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["counts"], json!({}));
    assert_eq!(empty["storage_ok"], true);
    assert_eq!(
        server
            .request("POST", "/api/diagnostics", &auth[..2], Some(batch(1)))
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/api/diagnostics",
                &[
                    ("Cookie", &primary),
                    ("Origin", "https://other.example"),
                    ("X-CSRF-Token", csrf)
                ],
                Some(batch(1))
            )
            .await
            .code,
        403
    );
    assert_eq!(
        server
            .request("DELETE", "/api/diagnostics", &auth, None)
            .await
            .code,
        405
    );
    let mut invalid = batch(1);
    invalid["events"][0]["message"] = json!("SECRET terminal content");
    assert_eq!(
        server
            .request("POST", "/api/diagnostics", &auth, Some(invalid))
            .await
            .code,
        400
    );
    let mut oversized = batch(1);
    oversized["private"] = json!("x".repeat(17000));
    assert_eq!(
        server
            .request("POST", "/api/diagnostics", &auth, Some(oversized))
            .await
            .code,
        400
    );
    for i in 0..6 {
        let mut headers = auth.to_vec();
        headers.push((
            "User-Agent",
            "Mozilla/5.0 (Macintosh) Version/18 Safari/605.1.15",
        ));
        assert_eq!(
            server
                .request(
                    "POST",
                    "/api/diagnostics",
                    &headers,
                    Some(batch(if i < 2 { 1 } else { i }))
                )
                .await
                .code,
            202
        );
    }
    let limited = server
        .request("POST", "/api/diagnostics", &auth, Some(batch(100)))
        .await;
    assert_eq!(limited.code, 429);
    assert!(limited
        .headers
        .contains(&("retry-after".into(), "60".into())));
    let result = server
        .request("GET", "/api/diagnostics", &auth[..1], None)
        .await;
    let exported = result.json();
    assert_eq!(exported["events"].as_array().unwrap().len(), 5);
    assert_eq!(exported["events"][0]["browser"], "Safari on macOS");
    for secret in [
        &primary,
        csrf,
        PASSWORD,
        "SECRET",
        access["login_id"].as_str().unwrap(),
    ] {
        assert!(!result.body.contains(secret));
    }
    assert!(exported["events"][0].get("account").is_none());
    assert!(exported["events"][0].get("profile").is_none());
    assert_eq!(
        server
            .request("GET", "/api/diagnostics", &[("Cookie", &guest)], None)
            .await
            .json()["events"],
        json!([])
    );
    server.stop().await;
    let path = fixture.root.join("credentials.json.diagnostics.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let server = fixture.start_all(None, false, false, true).await;
    let result = server
        .request("GET", "/api/diagnostics", &auth[..1], None)
        .await
        .json();
    assert_eq!(result["events"].as_array().unwrap().len(), 5);
    assert_eq!(result["pending_save"], false);
    // Stage a real slow POST, then revoke its cookie before completing the body.
    let mut pending = TcpStream::connect(server.address).await.unwrap();
    let raw = batch(100).to_string();
    let head=format!("POST /api/diagnostics HTTP/1.1\r\nHost: hmux.example\r\nOrigin: https://hmux.example\r\nCookie: {primary}\r\nX-CSRF-Token: {csrf}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",raw.len());
    pending.write_all(head.as_bytes()).await.unwrap();
    pending.write_all(&raw.as_bytes()[..1]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        server
            .request("POST", "/api/logout", &auth, None)
            .await
            .code,
        200
    );
    pending.write_all(&raw.as_bytes()[1..]).await.unwrap();
    let mut rejected = String::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        pending.read_to_string(&mut rejected),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(rejected.starts_with("HTTP/1.1 401"));
    let replacement = server.login("primary").await;
    assert_eq!(
        server
            .request("GET", "/api/diagnostics", &[("Cookie", &replacement)], None)
            .await
            .json()["events"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    server.stop().await;
    let raw = fs::read_to_string(path).unwrap();
    assert!(!raw.contains("SECRET"));
    assert!(!raw.contains(csrf));
}

#[tokio::test]
async fn invalid_diagnostics_file_is_reported_but_never_replaced() {
    let fixture = Fixture::new();
    let path = fixture.root.join("credentials.json.diagnostics.json");
    write_private(&path, "{broken synthetic}".into());
    let server = fixture.start_all(None, false, false, true).await;
    let cookie = server.login("primary").await;
    let access = server.session(&cookie).await.json();
    let csrf = access["csrf"].as_str().unwrap();
    let response = server
        .request("GET", "/api/diagnostics", &[("Cookie", &cookie)], None)
        .await;
    assert_eq!(response.code, 200);
    assert_eq!(response.json()["storage_ok"], false);
    assert_eq!(response.json()["events"], json!([]));
    assert_eq!(
        server
            .request(
                "POST",
                "/api/diagnostics",
                &[
                    ("Cookie", &cookie),
                    ("Origin", "https://hmux.example"),
                    ("X-CSRF-Token", csrf)
                ],
                Some(batch(1))
            )
            .await
            .code,
        503
    );
    server.stop().await;
    assert_eq!(fs::read_to_string(path).unwrap(), "{broken synthetic}");
}
