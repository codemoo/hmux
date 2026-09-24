use super::*;

#[tokio::test]
async fn public_assets_preserve_host_method_api_and_streaming_boundaries() {
    let fixture = Fixture::new();
    let assets = fixture.root.join("web");
    fs::create_dir(&assets).unwrap();
    fs::write(
        assets.join("index.html"),
        "<!doctype html><title>fixture</title>",
    )
    .unwrap();
    fs::write(assets.join("sw.js"), "// fixture worker").unwrap();
    fs::write(assets.join("manifest.json"), "{\"name\":\"fixture\"}").unwrap();
    fs::write(assets.join("large.txt"), "0123456789".repeat(300_000)).unwrap();
    let auth = Arc::new(open_auth(&fixture.credentials).await);
    let gateway = Arc::new(
        Gateway::new("https://hmux.example", CONNECTOR, auth)
            .unwrap()
            .with_assets(hmux_gateway::static_assets::Assets::open(&assets).unwrap()),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(gateway.serve(listener, shutdown.clone()));
    let server = Server {
        address,
        shutdown,
        task,
    };
    for path in ["/", "/sw.js", "/manifest.json"] {
        let get = server.request("GET", path, &[], None).await;
        assert_eq!(get.code, 200);
        assert!(get
            .headers
            .iter()
            .any(|(k, v)| k == "content-security-policy" && v == hmux_gateway::http_boundary::CSP));
        let head = server.request("HEAD", path, &[], None).await;
        assert_eq!(head.code, 200);
        assert!(head.body.is_empty());
        assert_eq!(
            head.headers.iter().find(|(k, _)| k == "content-length"),
            get.headers.iter().find(|(k, _)| k == "content-length")
        );
    }
    // Larger than the resident file-buffer budget; a real HTTP consumer drains
    // frames as they arrive instead of retaining the whole server-side file.
    let large = server.request("GET", "/large.txt", &[], None).await;
    assert_eq!(large.code, 200);
    assert_eq!(large.body, "0123456789".repeat(300_000));
    let range = server
        .request("GET", "/large.txt", &[("Range", "bytes=9-12")], None)
        .await;
    assert_eq!(range.code, 206);
    assert_eq!(range.body, "9012");
    assert_eq!(server.request("POST", "/", &[], None).await.code, 405);
    assert_eq!(
        server.request("GET", "/api/state", &[], None).await.code,
        401
    );
    assert_eq!(server.request("GET", "/connect", &[], None).await.code, 403);
    assert_eq!(
        server
            .request("GET", "/api%2Fsession", &[], None)
            .await
            .code,
        404
    );
    assert_eq!(server.request("HEAD", "/missing", &[], None).await.body, "");
    let mut invalid_host = TcpStream::connect(server.address).await.unwrap();
    invalid_host
        .write_all(b"GET / HTTP/1.1\r\nHost: elsewhere.example\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut raw = String::new();
    invalid_host.read_to_string(&mut raw).await.unwrap();
    assert!(raw.starts_with("HTTP/1.1 421"));
    // Disconnect with a body in progress; shutdown must join its actual IO.
    let mut stream = TcpStream::connect(server.address).await.unwrap();
    stream
        .write_all(b"GET /large.txt HTTP/1.1\r\nHost: hmux.example\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0; 256];
    assert!(stream.read(&mut first).await.unwrap() > 0);
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), server.stop())
        .await
        .unwrap();
}
