use super::*;
use file_io::{CHUNK_BYTES, RETAINED_CHUNKS, STREAMS};
use http_body_util::BodyExt;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::{self as disk, FileTimes},
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "hmux-e2e-static-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        disk::create_dir(&root).unwrap();
        Self(root)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        disk::create_dir_all(path.parent().unwrap()).unwrap();
        disk::write(path, bytes).unwrap();
    }
    fn assets(&self) -> Assets {
        Assets::open(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = disk::remove_dir_all(&self.0);
    }
}
fn request(method: &str, path: &str) -> Request<()> {
    Request::builder()
        .method(method)
        .uri(path)
        .body(())
        .unwrap()
}
async fn text(reply: Reply) -> String {
    String::from_utf8(
        reply
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn actual_go_static_oracle_matches_published_assets_and_http_semantics() {
    #[derive(Deserialize)]
    struct Oracle {
        files: BTreeMap<String, String>,
        modified: u64,
        cases: Vec<Case>,
    }
    #[derive(Deserialize, Debug)]
    struct Case {
        method: String,
        path: String,
        headers: Option<BTreeMap<String, String>>,
        status: u16,
        response: BTreeMap<String, String>,
        body: String,
    }
    let oracle: Oracle = serde_json::from_str(include_str!(
        "../../../tests/fixtures/static-v1/go-oracle.json"
    ))
    .unwrap();
    let fixture = Fixture::new();
    for (path, data) in &oracle.files {
        fixture.write(path, data.as_bytes());
        File::options()
            .write(true)
            .open(fixture.0.join(path))
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(oracle.modified)),
            )
            .unwrap();
    }
    let assets = fixture.assets();
    for case in oracle.cases {
        let mut req = request(&case.method, &case.path);
        for (key, value) in case.headers.as_ref().into_iter().flatten() {
            req.headers_mut().insert(
                key.parse::<header::HeaderName>().unwrap(),
                value.parse().unwrap(),
            );
        }
        let reply = assets.serve(&req).await;
        assert_eq!(reply.status().as_u16(), case.status, "{case:?}");
        assert_eq!(reply.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            reply.headers()[header::CONTENT_SECURITY_POLICY],
            boundary::CSP
        );
        if case.status >= 400 && case.status != 412 {
            // Existing shared candidate errors intentionally expose no filesystem
            // detail and preserve no-store; Go FileServer removes it on errors.
            assert_eq!(
                reply
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .map(|v| v.to_str().unwrap()),
                case.response.get("Content-Range").map(String::as_str)
            );
            if case.method == "HEAD" {
                assert!(text(reply).await.is_empty());
            }
            continue;
        }
        let (parts, body) = reply.into_parts();
        let mut observed: BTreeMap<String, String> = parts
            .headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap().to_owned(),
                )
            })
            .collect();
        let bytes = body.collect().await.unwrap().to_bytes();
        let mut contents = String::from_utf8(bytes.to_vec()).unwrap();
        if observed
            .get("content-type")
            .is_some_and(|v| v.starts_with("multipart/byteranges; boundary="))
        {
            observed.insert(
                "content-type".into(),
                "multipart/byteranges; boundary=BOUNDARY".into(),
            );
            let boundary = contents
                .lines()
                .next()
                .unwrap()
                .trim()
                .trim_start_matches("--")
                .to_owned();
            contents = contents.replace(&boundary, "BOUNDARY");
        }
        let expected: BTreeMap<_, _> = case
            .response
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        assert_eq!(observed, expected, "headers for {case:?}");
        assert_eq!(contents, case.body, "body for {case:?}");
    }
    assets.shutdown().await;
}

#[tokio::test]
async fn deployment_link_switch_is_live_and_inflight_file_stays_consistent() {
    let fixture = Fixture::new();
    fixture.write("a/index.html", b"release A");
    fixture.write("b/index.html", b"release B");
    let current = fixture.0.join("current");
    symlink("a", &current).unwrap();
    let assets = Assets::open(&current).unwrap();
    let old = assets.serve(&request("GET", "/")).await;
    symlink("b", fixture.0.join("next")).unwrap();
    disk::rename(fixture.0.join("next"), &current).unwrap();
    assert_eq!(
        text(assets.serve(&request("GET", "/")).await).await,
        "release B"
    );
    assert_eq!(text(old).await, "release A");
    assets.shutdown().await;
}

#[tokio::test]
async fn traversal_links_special_files_and_directory_listings_are_not_published() {
    let fixture = Fixture::new();
    fixture.write("index.html", b"fixture");
    fixture.write(".hidden", b"private fixture");
    fixture.write("api/session", b"reserved path fixture");
    fixture.write("connect", b"reserved path fixture");
    fixture.write("outside/secret.txt", b"secret fixture");
    disk::create_dir(fixture.0.join("listed")).unwrap();
    symlink("outside/secret.txt", fixture.0.join("link.txt")).unwrap();
    symlink("outside", fixture.0.join("escape")).unwrap();
    assert!(std::process::Command::new("/usr/bin/mkfifo")
        .arg(fixture.0.join("pipe.txt"))
        .status()
        .unwrap()
        .success());
    let assets = fixture.assets();
    for path in [
        "/api%2Fsession",
        "/conne%63t",
        "/../index.html",
        "/%2e%2e/index.html",
        "/%2Findex.html",
        "//index.html",
        "/.hidden",
        "/%00",
        "/%zz",
        "/index.html%5Cfoo",
        "/escape/secret.txt",
        "/link.txt",
        "/pipe.txt",
        "/listed/",
    ] {
        let reply =
            tokio::time::timeout(Duration::from_secs(1), assets.serve(&request("GET", path)))
                .await
                .unwrap();
        assert_eq!(reply.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let headers = assets.serve(&request("HEAD", "/missing")).await;
    assert!(text(headers).await.is_empty());
    assets.shutdown().await;
}

#[tokio::test]
async fn retained_file_chunks_are_bounded_through_body_drop_and_byte_slices() {
    let fixture = Fixture::new();
    fixture.write("index.html", b"fixture");
    File::create(fixture.0.join("large.bin"))
        .unwrap()
        .set_len((CHUNK_BYTES * (RETAINED_CHUNKS + 2)) as u64)
        .unwrap();
    let assets = fixture.assets();
    let mut body = assets
        .serve(&request("GET", "/large.bin"))
        .await
        .into_body();
    assert_eq!(assets.resources.chunks.available_permits(), RETAINED_CHUNKS);
    let mut retained = Vec::new();
    for _ in 0..RETAINED_CHUNKS {
        let bytes = body.frame().await.unwrap().unwrap().into_data().unwrap();
        assert_eq!(bytes.len(), CHUNK_BYTES);
        retained.push(bytes.slice(..1));
    }
    assert_eq!(assets.resources.chunks.available_permits(), 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), body.frame())
            .await
            .is_err()
    );
    drop(retained.pop());
    let bytes = tokio::time::timeout(Duration::from_secs(2), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(assets.resources.chunks.available_permits(), 0);
    drop(body);
    assert_eq!(assets.resources.streams.available_permits(), STREAMS);
    drop(bytes);
    drop(retained);
    assert_eq!(assets.resources.chunks.available_permits(), RETAINED_CHUNKS);
    assets.shutdown().await;
}

#[tokio::test]
async fn streams_head_ranges_and_shutdown_keep_admission_bounded() {
    let fixture = Fixture::new();
    fixture.write("index.html", b"fixture");
    let assets = fixture.assets();
    let mut responses = Vec::new();
    for _ in 0..STREAMS {
        let reply = assets.serve(&request("GET", "/")).await;
        assert_eq!(reply.status(), StatusCode::OK);
        responses.push(reply);
    }
    assert_eq!(
        assets.serve(&request("GET", "/")).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(responses);
    assert_eq!(assets.resources.streams.available_permits(), STREAMS);
    for _ in 0..STREAMS * 2 {
        let reply = assets.serve(&request("HEAD", "/")).await;
        assert_eq!(reply.status(), StatusCode::OK);
        assert!(text(reply).await.is_empty());
    }
    let mut req = request("GET", "/");
    req.headers_mut().insert(
        header::RANGE,
        format!("bytes={}", vec!["0-0"; MAX_RANGES + 1].join(","))
            .parse()
            .unwrap(),
    );
    let reply = assets.serve(&req).await;
    assert_eq!(reply.status(), StatusCode::OK);
    assert_eq!(text(reply).await, "fixture");
    assert_eq!(assets.resources.chunks.available_permits(), RETAINED_CHUNKS);
    assets.shutdown().await;
    assert_eq!(
        assets.serve(&request("GET", "/")).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn truncation_errors_release_resources_and_growth_cannot_extend_response() {
    let fixture = Fixture::new();
    fixture.write("index.html", b"fixture");
    fixture.write("mutable.txt", b"123456");
    let assets = fixture.assets();
    let reply = assets.serve(&request("GET", "/mutable.txt")).await;
    fixture.write("mutable.txt", b"12");
    assert!(reply.into_body().collect().await.is_err());
    assert_eq!(assets.resources.streams.available_permits(), STREAMS);
    assert_eq!(assets.resources.chunks.available_permits(), RETAINED_CHUNKS);
    let reply = assets.serve(&request("GET", "/mutable.txt")).await;
    fixture.write("mutable.txt", b"12345678");
    assert_eq!(text(reply).await, "12");
    assets.shutdown().await;
}
