//! Authenticated account lists -> fixed verified HTTPS metadata, then revoke
//! while lookup is stalled. Uses only disposable credentials and loopback TLS.
use super::*;

async fn login_at(auth: &AuthStore, name: &str, ip: &str) -> Browser {
    let result = auth
        .login(crate::auth_store::LoginRequest {
            username: name.into(),
            password: PASSWORD.into(),
            code: String::new(),
            source: ip.into(),
            ip: ip.into(),
            browser: "Synthetic browser".into(),
            now: Utc::now(),
        })
        .await;
    assert_eq!(result.status, crate::auth_store::LoginStatus::Succeeded);
    let token = result.token.unwrap();
    let access = auth
        .access(&token, false, Utc::now())
        .await
        .unwrap()
        .unwrap();
    Browser {
        cookie: format!("{}={token}", auth::COOKIE_NAME),
        csrf: access.csrf,
        id: access.id,
    }
}

#[tokio::test]
async fn session_locations_are_account_scoped_cached_and_revocation_cancels_lookup() {
    let fixture = Fixture::new();
    let auth = Arc::new(
        AuthStore::open(fixture.0.join("credentials.json"))
            .await
            .unwrap(),
    );
    let primary = login_at(&auth, "primary", "1.1.1.1").await;
    let _guest = login_at(&auth, "guest", "9.9.9.9").await;
    let (cert, key) = cert_for("ipwho.is", false);
    let peer = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let transport = client(cert.clone(), peer.local_addr().unwrap());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    let (started, ready) = oneshot::channel();
    let peer_task = tokio::spawn(async move {
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let mut started = Some(started);
        for index in 0..2 {
            let (socket, _) = peer.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            assert_eq!(socket.get_ref().1.server_name(), Some("ipwho.is"));
            let mut raw = Vec::new();
            let mut chunk = [0; 1024];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0);
                raw.extend_from_slice(&chunk[..n]);
                assert!(raw.len() < 8192);
            }
            let raw = String::from_utf8(raw).unwrap();
            let expected = if index == 0 { "1.1.1.1" } else { "8.8.8.8" };
            assert!(raw.starts_with(&format!(
                "GET /{expected}?fields=success,country,region,city HTTP/1.1\r\n"
            )));
            assert!(!raw.to_ascii_lowercase().contains("cookie:"));
            assert!(!raw.to_ascii_lowercase().contains("authorization:"));
            if index == 0 {
                let body = r#"{"success":true,"city":"City","region":"City","country":"Country"}"#;
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            } else {
                started.take().unwrap().send(()).unwrap();
                assert_eq!(socket.read(&mut chunk).await.unwrap_or(0), 0);
            }
        }
    });
    let gateway = Arc::new(
        Gateway::new(
            ORIGIN,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            auth.clone(),
        )
        .unwrap()
        .with_locations(crate::session_location::Locator::new(transport)),
    );
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let stop = CancellationToken::new();
    let server = BrowserServer {
        address: listener.local_addr().unwrap(),
        task: tokio::spawn(gateway.serve(listener, stop.clone())),
        stop,
    };
    assert_eq!(
        server
            .request(&Browser::default(), "GET", "/api/sessions", "")
            .await
            .status,
        401
    );
    for _ in 0..2 {
        let reply = server.request(&primary, "GET", "/api/sessions", "").await;
        assert_eq!(reply.status, 200);
        let rows = reply.body["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], primary.id);
        assert_eq!(rows[0]["location"], "City, Country");
    }
    let second = login_at(&auth, "primary", "8.8.8.8").await;
    let request = server.request(&second, "GET", "/api/sessions", "");
    let revoke = async {
        timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        server.post(&second, "/api/logout", json!({}), 200).await;
    };
    let (reply, _) = tokio::join!(request, revoke);
    assert_eq!(reply.status, 401);
    timeout(Duration::from_secs(2), peer_task)
        .await
        .unwrap()
        .unwrap();
    server.stop.cancel();
    server.task.await.unwrap().unwrap();
}
