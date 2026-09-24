//! Production `hmux-web serve` + `connect` smoke with synthetic private inputs.
//! The WSS proxy and fake tmux are loopback/local; no installed service or real
//! tmux socket, account, provider, or browser profile is touched.
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use hmux_gateway::auth::{self, Credentials};
use hmux_protocol::flow;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::{
    pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig,
};
use serde_json::{json, Value};
use std::{
    fs,
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    time::timeout,
};
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{client::IntoClientRequest, Message},
};

const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PASSWORD: &str = "synthetic-native-pair-password";
const WAIT: Duration = Duration::from_secs(15);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "hmux-e2e-native-pair-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        for name in ["private", "public"] {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path.join(name))
                .unwrap();
        }
        let fixture = Self(path);
        let salt = vec![5; 32];
        let creds = Credentials {
            username: "synthetic".into(),
            hash: auth::derive_password(PASSWORD, &salt).to_vec(),
            salt,
            totp_secret: data_encoding::BASE32_NOPAD.encode(&[5; 20]),
            last_step: 0,
            totp_disabled: true,
        };
        fixture.file("private/credentials.json", &creds.go_json(), 0o600);
        fixture.file("private/connector.token", TOKEN, 0o600);
        fixture.file("public/index.html", "synthetic public index", 0o600);
        fixture.file(
            "home.toml",
            &format!(
                "schema_version=1\nstate_dir='{}'\ninventory_path='{}'\n",
                fixture.0.join("state").display(),
                fixture.0.join("inventory.toml").display()
            ),
            0o600,
        );
        fixture.file(
            "inventory.toml",
            "schema_version=1\nrevision='synthetic'\n[[profiles]]\nid='shell'\nlabel='Shell'\ndefault_directory='~'\ncommand=['sh']\n",
            0o600,
        );
        fixture.file(
            "tmux",
            r#"#!/bin/sh
case "$1" in
list-sessions) printf '%s\n' '$7|:hmux-sep-v1:|synthetic|:hmux-sep-v1:|1700000000|:hmux-sep-v1:|1700000200|:hmux-sep-v1:|0|:hmux-sep-v1:|1|:hmux-sep-v1:||:hmux-sep-v1:|' ;;
list-windows) : ;;
list-panes) printf '%s\n' '$7|:hmux-recovery-v1:|@1|:hmux-recovery-v1:|0|:hmux-recovery-v1:|shell|:hmux-recovery-v1:|b1e2,80x24,0,0,0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|%0|:hmux-recovery-v1:|0|:hmux-recovery-v1:|1|:hmux-recovery-v1:|/synthetic|:hmux-recovery-v1:|123' ;;
display-message) printf '1700000000\n' ;;
new-session|set-hook|if-shell) : ;;
attach-session)
 stty -echo
 printf 'PAIR_READY\n'
 while IFS= read -r line; do printf 'PAIR_INPUT:%s\n' "$line"; done ;;
kill-session) printf 'unexpected kill-session\n' >> "${0%/*}/killed"; exit 1 ;;
*) exit 1 ;;
esac
"#,
            0o700,
        );
        fixture.file("ps", "#!/bin/sh\nexit 0\n", 0o700);
        fixture
    }
    fn file(&self, name: &str, contents: &str, mode: u32) {
        let path = self.0.join(name);
        fs::write(&path, contents).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn command(&self, binary: &Path) -> Command {
        let mut cmd = Command::new(binary);
        cmd.env_clear()
            .env("HOME", &self.0)
            .env(
                "PATH",
                std::env::join_paths([self.0.as_path(), Path::new("/usr/bin"), Path::new("/bin")])
                    .unwrap(),
            )
            .env("SSL_CERT_FILE", self.0.join("ca.pem"))
            .current_dir(&self.0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(
                fs::File::create(self.0.join("child.stderr")).unwrap(),
            ))
            .kill_on_drop(true);
        cmd
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn certificate(fixture: &Fixture) -> TlsAcceptor {
    let ca_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = params.self_signed(&ca_key).unwrap();
    fixture.file(
        "ca.pem",
        &format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            STANDARD.encode(ca.der())
        ),
        0o600,
    );
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .signed_by(&key, &ca, &ca_key)
        .unwrap();
    TlsAcceptor::from(Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap(),
    ))
}

async fn gateway(fixture: &Fixture, binary: &Path, origin: &str) -> (Child, SocketAddr) {
    let mut child = fixture.command(binary);
    child
        .arg("serve")
        .arg("--origin")
        .arg(origin)
        .arg("--credentials")
        .arg(fixture.0.join("private/credentials.json"))
        .arg("--token-file")
        .arg(fixture.0.join("private/connector.token"))
        .arg("--assets")
        .arg(fixture.0.join("public"))
        .arg("--listen")
        .arg("127.0.0.1:0");
    let mut child = child.spawn().unwrap();
    let mut line = String::new();
    timeout(
        WAIT,
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line),
    )
    .await
    .expect("gateway readiness timed out")
    .unwrap();
    let address = line
        .strip_prefix("HMux web listening on ")
        .and_then(|s| s.strip_suffix(" behind HTTPS\n"))
        .unwrap_or_else(|| panic!("unexpected gateway readiness: {line:?}"))
        .parse()
        .unwrap();
    (child, address)
}

async fn http(
    address: SocketAddr,
    host: SocketAddr,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<Value>,
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let payload = body.map_or(Vec::new(), |value| serde_json::to_vec(&value).unwrap());
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: https://{host}\r\nCookie: {cookie}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(request.as_bytes()).await.unwrap();
    socket.write_all(&payload).await.unwrap();
    let mut raw = Vec::new();
    timeout(WAIT, socket.read_to_end(&mut raw))
        .await
        .unwrap()
        .unwrap();
    let header_end = raw.windows(4).position(|x| x == b"\r\n\r\n").unwrap();
    let headers = String::from_utf8(raw[..header_end].to_vec()).unwrap();
    let mut lines = headers.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let fields = lines
        .map(|line| {
            let (name, value) = line.split_once(':').unwrap();
            (name.to_ascii_lowercase(), value.trim().to_owned())
        })
        .collect();
    (status, fields, raw[header_end + 4..].to_vec())
}

async fn state(address: SocketAddr, host: SocketAddr, cookie: &str) -> Value {
    let (code, _, body) = http(address, host, "GET", "/api/state", cookie, None).await;
    assert_eq!(code, 200);
    serde_json::from_slice(&body).unwrap()
}

async fn wait_state(
    fixture: &Fixture,
    address: SocketAddr,
    host: SocketAddr,
    cookie: &str,
    online: bool,
) -> Value {
    let mut observed = Value::Null;
    let result = timeout(WAIT, async {
        loop {
            let value = state(address, host, cookie).await;
            observed = value.clone();
            if value["online"] == online
                && (!online || value["catalog"]["sessions"][0]["id"] == "$7")
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    result.unwrap_or_else(|_| {
        panic!(
            "Home online={online} state transition timed out: {observed}; log={:?}; stderr={:?}",
            fs::read_to_string(fixture.0.join("home.log")),
            fs::read_to_string(fixture.0.join("child.stderr"))
        )
    })
}

async fn terminal(address: SocketAddr, host: SocketAddr, cookie: &str) {
    let mut request = format!("ws://{address}/api/terminal")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("host", host.to_string().parse().unwrap());
    request
        .headers_mut()
        .insert("origin", format!("https://{host}").parse().unwrap());
    request
        .headers_mut()
        .insert("cookie", cookie.parse().unwrap());
    let (mut ws, response) = client_async_with_config(
        request,
        TcpStream::connect(address).await.unwrap(),
        Some(hmux_gateway::browser_terminal::socket_config()),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 101);
    ws.send(Message::Text(json!({"type":"open","session":{"id":"$7","created_at":1700000000},"cols":80,"rows":24,"capabilities":[flow::CAPABILITY]}).to_string().into())).await.unwrap();
    let ready = timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
    let ready: Value = serde_json::from_slice(&ready.into_data()).unwrap();
    assert_eq!(ready["type"], "ready");
    assert_eq!(ready["output_flow"], true);
    let mut output = Vec::new();
    while !output
        .windows(b"PAIR_READY".len())
        .any(|w| w == b"PAIR_READY")
    {
        let frame = timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
        let Message::Binary(data) = frame else {
            panic!("terminal output expected")
        };
        output.extend_from_slice(&data);
        ws.send(Message::Text(
            json!({"type":"output-ack","received":data.len()})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    ws.send(Message::Binary(b"hello\n".to_vec().into()))
        .await
        .unwrap();
    output.clear();
    while !output
        .windows(b"PAIR_INPUT:hello".len())
        .any(|w| w == b"PAIR_INPUT:hello")
    {
        let frame = timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
        let Message::Binary(data) = frame else {
            panic!("terminal echo expected")
        };
        output.extend_from_slice(&data);
        ws.send(Message::Text(
            json!({"type":"output-ack","received":data.len()})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    }
    ws.close(None).await.unwrap();
}

async fn stop(child: Child) {
    let pid = rustix::process::Pid::from_raw(child.id().unwrap() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let output = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
}

#[tokio::test]
#[ignore = "set HMUX_NATIVE_WEB_BIN to a built hmux-web and run --ignored"]
async fn production_pair_login_catalog_terminal_ack_disconnect_and_reconnect() {
    let binary =
        PathBuf::from(std::env::var_os("HMUX_NATIVE_WEB_BIN").expect("HMUX_NATIVE_WEB_BIN"));
    assert!(binary.is_absolute());
    let fixture = Fixture::new();
    let tls = certificate(&fixture);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public = listener.local_addr().unwrap();
    let (gateway, backend) = gateway(&fixture, &binary, &format!("https://{public}")).await;
    let proxy = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let tls = tls.clone();
            tokio::spawn(async move {
                let Ok(mut front) = tls.accept(socket).await else {
                    return;
                };
                let Ok(mut back) = TcpStream::connect(backend).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut front, &mut back).await;
            });
        }
    });
    let (code, _, _) = http(backend, public, "GET", "/api/session", "", None).await;
    assert_eq!(code, 401);
    let (code, headers, _) = http(
        backend,
        public,
        "POST",
        "/api/login",
        "",
        Some(json!({"username":"synthetic","password":PASSWORD})),
    )
    .await;
    assert_eq!(code, 200);
    let cookie = headers
        .iter()
        .find(|(name, _)| name == "set-cookie")
        .unwrap()
        .1
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    for _ in 0..2 {
        let mut home = fixture.command(&binary);
        home.arg("connect")
            .arg("--url")
            .arg(format!("wss://{public}/connect"))
            .arg("--token-file")
            .arg(fixture.0.join("private/connector.token"))
            .arg("--config")
            .arg(fixture.0.join("home.toml"))
            .arg("--log-file")
            .arg(fixture.0.join("home.log"));
        let home = home.spawn().unwrap();
        let observed = wait_state(&fixture, backend, public, &cookie, true).await;
        assert_eq!(observed["catalog"]["sessions"][0]["created_at"], 1700000000);
        terminal(backend, public, &cookie).await;
        assert!(!fixture.0.join("killed").exists());
        assert_eq!(
            state(backend, public, &cookie).await["catalog"]["sessions"][0]["id"],
            "$7"
        );
        stop(home).await;
        wait_state(&fixture, backend, public, &cookie, false).await;
    }
    stop(gateway).await;
    proxy.abort();
}
