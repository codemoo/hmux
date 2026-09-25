//! Native Gateway assembly, including one-time web enrollment and bounded cleanup.
use crate::{
    auth_store::AuthStore,
    bootstrap, diagnostics,
    http_auth::Gateway,
    http_boundary::{loopback_address, Policy},
    hub::Hub,
    push::Push,
    push_state, push_transport,
    static_assets::Assets,
    usage_preferences,
};
use hmux_core::{workspace, PrivateDir};
use std::{
    ffi::{OsStr, OsString},
    io,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const DUMMY_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub struct Options {
    pub origin: String,
    pub credentials: PathBuf,
    pub token_file: PathBuf,
    pub assets: PathBuf,
    pub listen: SocketAddr,
}
impl Options {
    /// CLI parser shared by the candidate executable and later CLI integration.
    /// Required paths are explicit; no production Home or account defaults apply.
    pub fn parse(args: impl IntoIterator<Item = String>) -> io::Result<Self> {
        let mut args = args.into_iter();
        let mut experimental = false;
        let mut origin = None;
        let mut credentials = None;
        let mut token_file = None;
        let mut assets = None;
        let mut listen = None;
        while let Some(flag) = args.next() {
            if flag == "--experimental-gateway" && !experimental {
                experimental = true;
                continue;
            }
            let slot = match flag.as_str() {
                "--origin" => &mut origin,
                "--credentials" => &mut credentials,
                "--token-file" => &mut token_file,
                "--assets" => &mut assets,
                "--listen" => &mut listen,
                _ => return Err(invalid("unknown or duplicate candidate option")),
            };
            let value = args.next().ok_or_else(|| invalid("missing option value"))?;
            if value.starts_with("--") || slot.replace(value).is_some() {
                return Err(invalid("missing or duplicate option value"));
            }
        }
        if !experimental {
            return Err(invalid("--experimental-gateway is required"));
        }
        let origin = origin.ok_or_else(|| invalid("--origin is required"))?;
        Policy::new(&origin, DUMMY_TOKEN).map_err(|_| invalid("invalid HTTPS origin"))?;
        let credentials =
            absolute(credentials.ok_or_else(|| invalid("--credentials is required"))?)?;
        let token_file = absolute(token_file.ok_or_else(|| invalid("--token-file is required"))?)?;
        let assets = absolute(assets.ok_or_else(|| invalid("--assets is required"))?)?;
        let listen = loopback_address(listen.as_deref().unwrap_or("127.0.0.1:8088"))?;
        Ok(Self {
            origin,
            credentials,
            token_file,
            assets,
            listen,
        })
    }
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn unavailable(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn absolute(value: String) -> io::Result<PathBuf> {
    let path = PathBuf::from(value);
    if !path.is_absolute()
        || !path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(invalid("candidate paths must be absolute and normalized"));
    }
    Ok(path)
}
fn credential_location(path: &Path) -> io::Result<(&Path, &OsStr)> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("invalid credentials path"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("invalid credentials name"))?;
    Ok((parent, name))
}
fn sibling_name(base: &OsStr, suffix: &str) -> OsString {
    let mut name = base.to_os_string();
    name.push(suffix);
    name
}
pub(crate) fn asset_boundary(options: &Options) -> io::Result<Vec<PathBuf>> {
    // The administrator chooses public release files. Reject an accidental
    // root containing either private bearer file at startup. Assets itself
    // still reopens a trusted deployment symlink for every request.
    let assets = options
        .assets
        .canonicalize()
        .map_err(|_| unavailable("public assets unavailable"))?;
    let mut protected = Vec::new();
    for private in [
        &options.credentials,
        &options.token_file,
        &bootstrap::token_path(&options.credentials),
    ] {
        let private = match private.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = private
                    .parent()
                    .ok_or_else(|| invalid("invalid private path"))?;
                parent.canonicalize()?.join(
                    private
                        .file_name()
                        .ok_or_else(|| invalid("invalid private name"))?,
                )
            }
            Err(_) => return Err(unavailable("private gateway input unavailable")),
        };
        if private.starts_with(&assets) {
            return Err(invalid("public assets include private gateway state"));
        }
        protected.push(private);
    }
    let credentials = &protected[0];
    let parent = credentials
        .parent()
        .ok_or_else(|| invalid("invalid private path"))?;
    let name = credentials
        .file_name()
        .ok_or_else(|| invalid("invalid private name"))?;
    let preferences = parent.join(sibling_name(name, ".usage-preferences"));
    let profiles = parent.join("web-profiles");
    let users = parent.join(sibling_name(name, ".users"));
    let backups = parent.join(sibling_name(name, ".backups"));
    protected.extend([preferences, profiles, users, backups]);
    Ok(protected)
}
async fn read_token(path: PathBuf) -> io::Result<String> {
    let token = tokio::task::spawn_blocking(move || hmux_core::token::load(&path))
        .await
        .map_err(|_| unavailable("connector token reader unavailable"))??;
    Ok(token)
}

struct Startup {
    auth: Arc<AuthStore>,
    diagnostics: Option<diagnostics::Store>,
    push: Option<push_state::Store>,
    preferences: Option<usage_preferences::Store>,
    workspaces: Option<workspace::Store>,
}
impl Startup {
    async fn shutdown(self) {
        if let Some(push) = self.push {
            push.shutdown().await;
        }
        if let Some(diagnostics) = self.diagnostics {
            diagnostics.shutdown().await;
        }
        if let Some(workspaces) = self.workspaces {
            workspaces.shutdown().await;
        }
        if let Some(preferences) = self.preferences {
            preferences.shutdown().await;
        }
        self.auth.shutdown().await;
    }
}

pub struct GatewayRuntime {
    gateway: RuntimeGateway,
    listener: TcpListener,
}
enum RuntimeGateway {
    Normal(Arc<Gateway>),
    Bootstrap(Arc<bootstrap::BootstrapGateway>),
}
impl GatewayRuntime {
    /// Validate trust and bind before opening private state. Any later failure
    /// explicitly drains initialized owners before returning.
    pub async fn open(options: Options) -> io::Result<Self> {
        Self::open_reported(options, None).await
    }
    pub async fn open_reported(
        options: Options,
        reporter: Option<crate::observation::Reporter>,
    ) -> io::Result<Self> {
        let client = push_transport::Client::new()
            .await
            .map_err(|_| unavailable("native push trust unavailable"))?;
        Self::open_with_client_reported(options, client, reporter).await
    }

    /// Concrete transport injection supports isolated verified-TLS tests while
    /// production `open` always constructs its own native-root client.
    pub async fn open_with_client(
        options: Options,
        client: push_transport::Client,
    ) -> io::Result<Self> {
        Self::open_with_client_reported(options, client, None).await
    }
    async fn open_with_client_reported(
        options: Options,
        client: push_transport::Client,
        reporter: Option<crate::observation::Reporter>,
    ) -> io::Result<Self> {
        Policy::new(&options.origin, DUMMY_TOKEN).map_err(|_| invalid("invalid HTTPS origin"))?;
        loopback_address(&options.listen.to_string())?;
        credential_location(&options.credentials)?;
        credential_location(&options.token_file)?;
        let protected = asset_boundary(&options)?;
        let assets = Assets::open_excluding(&options.assets, protected)?;
        let listener = TcpListener::bind(options.listen).await?;
        let token = read_token(options.token_file.clone()).await?;
        Policy::new(&options.origin, &token).map_err(|_| invalid("invalid gateway policy"))?;
        if !options.credentials.exists() {
            let setup =
                bootstrap::BootstrapGateway::open(options, token, assets, client, reporter)?;
            return Ok(Self {
                gateway: RuntimeGateway::Bootstrap(Arc::new(setup)),
                listener,
            });
        }
        let gateway = match build_gateway(&options, &token, assets, client.clone(), reporter).await
        {
            Ok(gateway) => gateway,
            Err(error) => {
                client.shutdown();
                return Err(error);
            }
        };
        Ok(Self {
            gateway: RuntimeGateway::Normal(Arc::new(gateway)),
            listener,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
    pub async fn serve(self, shutdown: CancellationToken) -> io::Result<()> {
        match self.gateway {
            RuntimeGateway::Normal(gateway) => gateway.serve(self.listener, shutdown).await,
            RuntimeGateway::Bootstrap(gateway) => gateway.serve(self.listener, shutdown).await,
        }
    }
    /// For callers that opened the candidate but did not start its listener.
    pub async fn shutdown(self) {
        match self.gateway {
            RuntimeGateway::Normal(gateway) => gateway.shutdown_services().await,
            RuntimeGateway::Bootstrap(gateway) => gateway.shutdown().await,
        }
    }
}

pub(crate) async fn build_gateway(
    options: &Options,
    token: &str,
    assets: Assets,
    client: push_transport::Client,
    reporter: Option<crate::observation::Reporter>,
) -> io::Result<Gateway> {
    let (credential_dir, credential_name) = credential_location(&options.credentials)?;
    let auth = Arc::new(AuthStore::open(&options.credentials).await?);
    let mut startup = Startup {
        auth,
        diagnostics: None,
        push: None,
        preferences: None,
        workspaces: None,
    };
    let result: io::Result<()> = async {
        let name = sibling_name(credential_name, ".diagnostics.json");
        startup.diagnostics = Some(
            diagnostics::Store::open(PrivateDir::open(credential_dir)?, name)
                .await
                .map_err(|_| unavailable("private diagnostics unavailable"))?,
        );
        startup.push = Some(
            push_state::Store::open(PrivateDir::open(credential_dir)?, credential_name)
                .await
                .map_err(|_| unavailable("private push storage unavailable"))?,
        );
        let preferences_dir = PrivateDir::open(credential_dir)?
            .create_private_child(&sibling_name(credential_name, ".usage-preferences"))?;
        startup.preferences = Some(usage_preferences::Store::new(preferences_dir));
        let workspaces_dir =
            PrivateDir::open(credential_dir)?.create_private_child(OsStr::new("web-profiles"))?;
        startup.workspaces = Some(workspace::Store::new(workspaces_dir));
        Ok(())
    }
    .await;
    if let Err(error) = result {
        startup.shutdown().await;
        assets.shutdown().await;
        return Err(error);
    }
    let (hub, receiver) = Hub::with_reporter(reporter);
    let locations = crate::session_location::Locator::new(client.clone());
    let push = Push::new(
        startup.push.take().expect("push initialized"),
        client,
        receiver,
    );
    let gateway = Gateway::new(&options.origin, token, startup.auth.clone())
        .expect("validated gateway policy")
        .with_locations(locations)
        .with_home(hub)
        .with_assets(assets)
        .with_diagnostics(startup.diagnostics.take().expect("diagnostics initialized"))
        .with_preferences(startup.preferences.take().expect("preferences initialized"))
        .with_workspaces(startup.workspaces.take().expect("workspaces initialized"))
        .with_push(push);
    Ok(gateway)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{self, Credentials};
    use hmac::{Hmac, Mac};
    use sha1::Sha1;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        credentials: PathBuf,
        token: PathBuf,
        assets: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "hmux-e2e-rust-gateway-runtime-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            private_dir(&root);
            let private = root.join("private");
            private_dir(&private);
            let assets = root.join("public");
            private_dir(&assets);
            let credentials = private.join("credentials.json");
            let salt = vec![5; 32];
            let credential = Credentials {
                username: "primary".into(),
                hash: auth::derive_password("synthetic-password", &salt).to_vec(),
                salt,
                totp_secret: data_encoding::BASE32_NOPAD.encode(&[5; 20]),
                last_step: 0,
                totp_disabled: true,
            };
            private_file(&credentials, credential.go_json().as_bytes());
            let token = private.join("connector.token");
            private_file(&token, format!("{DUMMY_TOKEN}\n").as_bytes());
            fs::write(assets.join("index.html"), b"synthetic public index").unwrap();
            Self {
                root,
                credentials,
                token,
                assets,
            }
        }
        fn options(&self) -> Options {
            Options {
                origin: "https://hmux.example".into(),
                credentials: self.credentials.clone(),
                token_file: self.token.clone(),
                assets: self.assets.clone(),
                listen: "127.0.0.1:0".parse().unwrap(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn private_dir(path: &Path) {
        fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
    fn private_file(path: &Path, data: &[u8]) {
        fs::write(path, data).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn synthetic_client() -> push_transport::Client {
        let certified = rcgen::generate_simple_self_signed(vec!["hmux.example".into()]).unwrap();
        push_transport::Client::with_test_roots(vec![certified.cert.der().clone()]).unwrap()
    }
    async fn request(address: SocketAddr, raw: &str) -> String {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket.write_all(raw.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            socket.read_to_end(&mut answer),
        )
        .await
        .unwrap()
        .unwrap();
        String::from_utf8(answer).unwrap()
    }
    fn code(response: &str) -> &str {
        response.split_whitespace().nth(1).unwrap()
    }
    fn setup_post(path: &str, body: &str, origin: &str) -> String {
        format!("POST {path} HTTP/1.1\r\nHost: hmux.example\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }
    fn body_json(response: &str) -> serde_json::Value {
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }
    fn totp_code(secret: &str) -> String {
        let secret = auth::decode_totp_secret(secret).unwrap();
        let step = chrono::Utc::now().timestamp() / 30;
        let mut mac = Hmac::<Sha1>::new_from_slice(&secret).unwrap();
        mac.update(&(step as u64).to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let offset = (digest[19] & 15) as usize;
        let number = (u32::from_be_bytes(digest[offset..offset + 4].try_into().unwrap())
            & 0x7fff_ffff)
            % 1_000_000;
        format!("{number:06}")
    }

    #[tokio::test]
    async fn browser_setup_without_totp_switches_to_regular_and_survives_restart() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.credentials).unwrap();
        fs::remove_file(&fixture.token).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        let setup_meta = fs::metadata(bootstrap::token_path(&fixture.credentials)).unwrap();
        let connector_meta = fs::metadata(&fixture.token).unwrap();
        assert_eq!(setup_meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(connector_meta.permissions().mode() & 0o777, 0o600);
        let setup_token =
            hmux_core::token::load(&bootstrap::token_path(&fixture.credentials)).unwrap();
        let connector_token = hmux_core::token::load(&fixture.token).unwrap();
        assert_ne!(setup_token, connector_token);
        private_file(
            &bootstrap::token_path(&fixture.credentials),
            connector_token.as_bytes(),
        );
        assert!(bootstrap::initialize(&fixture.credentials, &fixture.token).is_err());
        assert!(
            GatewayRuntime::open_with_client(fixture.options(), synthetic_client())
                .await
                .is_err()
        );
        private_file(
            &bootstrap::token_path(&fixture.credentials),
            setup_token.as_bytes(),
        );
        let runtime = GatewayRuntime::open_with_client(fixture.options(), synthetic_client())
            .await
            .unwrap();
        let addr = runtime.local_addr().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(runtime.serve(stop.clone()));
        let status = request(
            addr,
            "GET /api/setup/status HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(body_json(&status)["required"], true);
        let session = request(
            addr,
            "GET /api/session HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(code(&session), "401");
        let assets = request(
            addr,
            "GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(code(&assets), "200");
        let connect = request(addr, &format!("GET /connect HTTP/1.1\r\nHost: hmux.example\r\nAuthorization: Bearer {connector_token}\r\nConnection: close\r\n\r\n")).await;
        assert_eq!(code(&connect), "503");
        let login = request(
            addr,
            &setup_post("/api/login", "{}", "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&login), "503");
        let start = serde_json::json!({"token": setup_token, "username":"owner", "password":"synthetic-password", "password_confirm":"synthetic-password", "totp_enabled":false}).to_string();
        let foreign = request(
            addr,
            &setup_post("/api/setup/begin", &start, "https://evil.example"),
        )
        .await;
        assert_eq!(code(&foreign), "403");
        let wrong = start.replace(&setup_token, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let denied = request(
            addr,
            &setup_post("/api/setup/begin", &wrong, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&denied), "401");
        let finished = request(
            addr,
            &setup_post("/api/setup/begin", &start, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&finished), "200");
        assert_eq!(body_json(&finished)["complete"], true);
        assert!(finished
            .to_ascii_lowercase()
            .contains("cache-control: no-store"));
        assert!(!bootstrap::token_path(&fixture.credentials).exists());
        let status = request(
            addr,
            "GET /api/setup/status HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(body_json(&status)["required"], false);
        let retry = request(
            addr,
            &setup_post("/api/setup/begin", &start, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&retry), "409");
        let login = request(
            addr,
            &setup_post(
                "/api/login",
                "{\"username\":\"owner\",\"password\":\"synthetic-password\",\"code\":\"\"}",
                "https://hmux.example",
            ),
        )
        .await;
        assert_eq!(code(&login), "200");
        stop.cancel();
        serving.await.unwrap().unwrap();
        let restarted = GatewayRuntime::open_with_client(fixture.options(), synthetic_client())
            .await
            .unwrap();
        let addr = restarted.local_addr().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(restarted.serve(stop.clone()));
        let status = request(
            addr,
            "GET /api/setup/status HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(body_json(&status)["required"], false);
        stop.cancel();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn browser_setup_requires_totp_and_retires_enrollment() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.credentials).unwrap();
        fs::remove_file(&fixture.token).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        let setup_token =
            hmux_core::token::load(&bootstrap::token_path(&fixture.credentials)).unwrap();
        let runtime = GatewayRuntime::open_with_client(fixture.options(), synthetic_client())
            .await
            .unwrap();
        let addr = runtime.local_addr().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(runtime.serve(stop.clone()));
        let start = serde_json::json!({"token": setup_token, "username":"owner", "password":"synthetic-password", "password_confirm":"synthetic-password", "totp_enabled":true}).to_string();
        let begun = request(
            addr,
            &setup_post("/api/setup/begin", &start, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&begun), "200");
        let body = body_json(&begun);
        assert_eq!(body["complete"], false);
        assert!(body["totp_uri"]
            .as_str()
            .unwrap()
            .contains(body["totp_secret"].as_str().unwrap()));
        assert!(!fixture.credentials.exists());
        let old_id = body["enrollment_id"].as_str().unwrap().to_owned();
        let replaced = request(
            addr,
            &setup_post("/api/setup/begin", &start, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&replaced), "200");
        let body = body_json(&replaced);
        let id = body["enrollment_id"].as_str().unwrap();
        assert_ne!(id, old_id);
        let obsolete = serde_json::json!({"token":setup_token,"enrollment_id":old_id,"code":totp_code(body["totp_secret"].as_str().unwrap())}).to_string();
        let denied_old = request(
            addr,
            &setup_post("/api/setup/complete", &obsolete, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&denied_old), "401");
        assert!(!fixture.credentials.exists());
        let invalid =
            serde_json::json!({"token":setup_token,"enrollment_id":id,"code":"00000"}).to_string();
        let denied = request(
            addr,
            &setup_post("/api/setup/complete", &invalid, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&denied), "401");
        let correct = serde_json::json!({"token":setup_token,"enrollment_id":id,"code":totp_code(body["totp_secret"].as_str().unwrap())}).to_string();
        let finished = request(
            addr,
            &setup_post("/api/setup/complete", &correct, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&finished), "200");
        assert_eq!(body_json(&finished)["complete"], true);
        let credential = auth::parse_credentials(&fs::read(&fixture.credentials).unwrap()).unwrap();
        assert!(!credential.totp_disabled);
        assert!(credential.last_step > 0);
        assert!(!bootstrap::token_path(&fixture.credentials).exists());
        stop.cancel();
        serving.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn browser_setup_limits_token_guesses_without_kdf_work() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.credentials).unwrap();
        fs::remove_file(&fixture.token).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        let runtime = GatewayRuntime::open_with_client(fixture.options(), synthetic_client())
            .await
            .unwrap();
        let addr = runtime.local_addr().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(runtime.serve(stop.clone()));
        let wrong = serde_json::json!({"token":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "username":"owner", "password":"synthetic-password", "password_confirm":"synthetic-password", "totp_enabled":false}).to_string();
        for _ in 0..10 {
            let denied = request(
                addr,
                &setup_post("/api/setup/begin", &wrong, "https://hmux.example"),
            )
            .await;
            assert_eq!(code(&denied), "401");
        }
        let limited = request(
            addr,
            &setup_post("/api/setup/begin", &wrong, "https://hmux.example"),
        )
        .await;
        assert_eq!(code(&limited), "429");
        assert!(!fixture.credentials.exists());
        stop.cancel();
        serving.await.unwrap().unwrap();
    }

    #[test]
    fn init_web_rejects_unsafe_partial_state_and_existing_accounts() {
        let fixture = Fixture::new();
        let old = fs::read(&fixture.credentials).unwrap();
        assert_eq!(
            bootstrap::initialize(&fixture.credentials, &fixture.token)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&fixture.credentials).unwrap(), old);
        fs::remove_file(&fixture.credentials).unwrap();
        fs::remove_file(&fixture.token).unwrap();
        let setup = bootstrap::token_path(&fixture.credentials);
        private_file(&setup, b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n");
        assert!(bootstrap::initialize(&fixture.credentials, &fixture.token).is_err());
        assert!(!fixture.token.exists());
        fs::remove_file(&setup).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        assert!(hmux_core::token::load(&setup).is_ok());
        let original_connector = hmux_core::token::load(&fixture.token).unwrap();
        fs::remove_file(&setup).unwrap();
        bootstrap::initialize(&fixture.credentials, &fixture.token).unwrap();
        assert!(hmux_core::token::load(&setup).is_ok());
        assert_eq!(
            hmux_core::token::load(&fixture.token).unwrap(),
            original_connector
        );
    }

    #[test]
    fn options_require_explicit_role_paths_origin_and_loopback() {
        let valid = [
            "--experimental-gateway",
            "--origin",
            "https://hmux.example",
            "--credentials",
            "/private/credentials.json",
            "--token-file",
            "/private/token",
            "--assets",
            "/public/web",
        ];
        let parse = |args: &[&str]| Options::parse(args.iter().map(|s| (*s).to_owned()));
        assert!(parse(&valid).is_ok());
        assert_eq!(
            parse(&valid).unwrap().listen,
            "127.0.0.1:8088".parse().unwrap()
        );
        for changed in [
            valid[1..].to_vec(),
            [valid.as_slice(), &["--origin", "https://other.example"]].concat(),
            [valid.as_slice(), &["--unexpected", "value"]].concat(),
            [valid.as_slice(), &["--listen", "0.0.0.0:8088"]].concat(),
            [valid.as_slice(), &["--listen", "localhost:8088"]].concat(),
        ] {
            assert!(parse(&changed).is_err(), "{changed:?}");
        }
        let mut relative = valid;
        relative[4] = "credentials.json";
        assert!(parse(&relative).is_err());
        let mut http_origin = valid;
        http_origin[2] = "http://hmux.example";
        assert!(parse(&http_origin).is_err());
        let mut mixed_path = valid;
        mixed_path[8] = "/public/../private";
        assert!(parse(&mixed_path).is_err());
    }

    #[test]
    fn public_assets_cannot_contain_private_inputs() {
        let fixture = Fixture::new();
        assert!(asset_boundary(&fixture.options()).is_ok());
        let mut unsafe_options = fixture.options();
        unsafe_options.assets = fixture.root.clone();
        assert!(asset_boundary(&unsafe_options).is_err());
        unsafe_options.assets = fixture.root.join("private");
        assert!(asset_boundary(&unsafe_options).is_err());
    }

    #[tokio::test]
    async fn full_startup_routes_and_shutdown_release_push_lock() {
        let fixture = Fixture::new();
        let client = synthetic_client();
        let link = fixture.root.join("current");
        std::os::unix::fs::symlink(&fixture.assets, &link).unwrap();
        let mut options = fixture.options();
        options.assets = link.clone();
        let runtime = GatewayRuntime::open_with_client(options, client)
            .await
            .unwrap();
        let address = runtime.local_addr().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(runtime.serve(stop.clone()));
        let home = request(
            address,
            "GET /connect HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(code(&home), "403");
        let assets = request(
            address,
            "GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(code(&assets), "200");
        assert!(assets.contains("synthetic public index"));
        // A live release switch must never turn private state into public files.
        let private = fixture.credentials.parent().unwrap();
        private_file(&private.join("index.html"), b"private index");
        for (target, resource) in [
            (private.to_path_buf(), "credentials.json"),
            (private.join("web-profiles"), "account.json"),
            (private.join("credentials.json.users"), "guest.json"),
            (private.join("credentials.json.backups"), "retained.json"),
            (
                private.join("credentials.json.usage-preferences"),
                "account.json",
            ),
        ] {
            if !target.exists() {
                private_dir(&target);
            }
            if resource != "credentials.json" {
                private_file(&target.join(resource), b"private account state");
            }
            fs::remove_file(&link).unwrap();
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let response = request(
                address,
                &format!(
                    "GET /{resource} HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n"
                ),
            )
            .await;
            assert_eq!(code(&response), "404");
            assert!(!response.contains("private account state"));
        }
        // Valid public releases still change without a process restart.
        let next = fixture.root.join("next-public");
        private_dir(&next);
        fs::write(next.join("index.html"), b"next public release").unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&next, &link).unwrap();
        let next = request(
            address,
            "GET / HTTP/1.1\r\nHost: hmux.example\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert_eq!(code(&next), "200");
        assert!(next.contains("next public release"));
        let body = r#"{"username":"primary","password":"synthetic-password"}"#;
        let login = request(address, &format!(
            "POST /api/login HTTP/1.1\r\nHost: hmux.example\r\nOrigin: https://hmux.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
        )).await;
        assert_eq!(code(&login), "200");
        let cookie = login
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
            .unwrap()
            .split_once(':')
            .unwrap()
            .1
            .trim()
            .split(';')
            .next()
            .unwrap();
        let sessions = request(address, &format!(
            "GET /api/sessions HTTP/1.1\r\nHost: hmux.example\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        )).await;
        assert_eq!(code(&sessions), "200");
        assert!(sessions.contains("내부·예약 네트워크"));
        let push = request(address, &format!(
            "GET /api/push HTTP/1.1\r\nHost: hmux.example\r\nCookie: {cookie}\r\nConnection: close\r\n\r\n"
        )).await;
        assert_eq!(code(&push), "200");
        assert!(push.contains("public_key"));
        stop.cancel();
        serving.await.unwrap().unwrap();
        let reopened = push_state::Store::open(
            PrivateDir::open(fixture.credentials.parent().unwrap()).unwrap(),
            OsStr::new("credentials.json"),
        )
        .await
        .unwrap();
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn failure_after_diagnostics_start_drains_and_bind_conflict_precedes_state() {
        let fixture = Fixture::new();
        let client = synthetic_client();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut options = fixture.options();
        options.listen = listener.local_addr().unwrap();
        assert!(GatewayRuntime::open_with_client(options, client.clone())
            .await
            .is_err());
        assert!(!fixture
            .credentials
            .with_extension("json.push.json")
            .exists());
        drop(listener);
        let push_file = fixture.root.join("private/credentials.json.push.json");
        private_file(&push_file, b"{invalid");
        assert!(GatewayRuntime::open_with_client(fixture.options(), client)
            .await
            .is_err());
        fs::remove_file(&push_file).unwrap();
        let reopened = push_state::Store::open(
            PrivateDir::open(fixture.credentials.parent().unwrap()).unwrap(),
            OsStr::new("credentials.json"),
        )
        .await
        .unwrap();
        reopened.shutdown().await;
        // The failed startup also drained auth and diagnostics; a full owner
        // can take the same private state after the malformed push file is fixed.
        let client = synthetic_client();
        let runtime = GatewayRuntime::open_with_client(fixture.options(), client)
            .await
            .unwrap();
        runtime.shutdown().await;
    }
}
