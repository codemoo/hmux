//! One-time, browser-driven primary account enrollment. The bootstrap bearer is
//! independent of the Home connector bearer and is never accepted by /connect.
use crate::{
    auth::{self, Credentials},
    http_auth::Gateway,
    http_boundary::{self as boundary, Policy, Reply, RequestContext},
    observation::Reporter,
    push_transport,
    runtime::{self, Options},
    static_assets::Assets,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmux_core::{token, PrivateDir, WriteError};
use http::{Method, Request, StatusCode};
use hyper::body::Incoming;
use serde::Deserialize;
use serde_json::json;
use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, sync::Mutex};
use tokio_util::sync::CancellationToken;

const PENDING_LIFETIME: Duration = Duration::from_secs(300);
const LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u8 = 10;

pub fn token_path(credentials: &Path) -> PathBuf {
    let mut path = credentials.as_os_str().to_os_string();
    path.push(".bootstrap");
    PathBuf::from(path)
}

/// Idempotently prepare a new web enrollment. Existing credentials always win;
/// a partial connector-token-only install is resumed with a fresh setup token.
/// Neither bearer is printed, passed in argv, or placed in account credentials.
pub fn initialize(credentials: &Path, connector_token: &Path) -> io::Result<()> {
    clean_path(credentials)?;
    clean_path(connector_token)?;
    let setup_path = token_path(credentials);
    if credentials == connector_token || setup_path == connector_token {
        return Err(invalid("secret paths must differ"));
    }
    let credential_dir = private_parent(credentials, true)?;
    let connector_dir = private_parent(connector_token, true)?;
    let name = credentials
        .file_name()
        .ok_or_else(|| invalid("invalid credentials path"))?;
    match credential_dir.read_private(name, 8192) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "credentials already exist",
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let setup_name = setup_path
        .file_name()
        .ok_or_else(|| invalid("invalid setup path"))?;
    let connector_name = connector_token
        .file_name()
        .ok_or_else(|| invalid("invalid connector path"))?;
    let setup_exists = match credential_dir.read_private(setup_name, 256) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let connector_exists = match connector_dir.read_private(connector_name, 256) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    if setup_exists && !connector_exists {
        return Err(invalid("incomplete private enrollment state"));
    }
    create_or_validate_token(&connector_dir, connector_name)?;
    create_or_validate_token(&credential_dir, setup_name)?;
    if token::load(&setup_path)? == token::load(connector_token)? {
        return Err(invalid("setup and connector tokens must be distinct"));
    }
    Ok(())
}

fn clean_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || !path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        || path.file_name().is_none()
    {
        return Err(invalid("secret paths must be clean absolute paths"));
    }
    Ok(())
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn private_parent(path: &Path, create: bool) -> io::Result<PrivateDir> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("invalid secret path"))?;
    if create {
        PrivateDir::open_or_create_trusted(parent)
    } else {
        PrivateDir::open_existing_trusted(parent)
    }
}
fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| io::Error::other("secure randomness unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
fn create_or_validate_token(dir: &PrivateDir, name: &OsStr) -> io::Result<()> {
    match dir.read_private(name, 256) {
        Ok(raw) => {
            let value = std::str::from_utf8(&raw)
                .map_err(|_| invalid("invalid private token"))?
                .trim();
            if !token::valid(value) {
                return Err(invalid("invalid private token"));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let value = format!("{}\n", random_token()?);
            match dir.write_new_private(name, value.as_bytes()) {
                Ok(()) => Ok(()),
                Err(WriteError::BeforeCommit(error))
                    if error.kind() == io::ErrorKind::AlreadyExists =>
                {
                    let raw = dir.read_private(name, 256)?;
                    let value = std::str::from_utf8(&raw)
                        .map_err(|_| invalid("invalid private token"))?
                        .trim();
                    if token::valid(value) {
                        Ok(())
                    } else {
                        Err(invalid("invalid private token"))
                    }
                }
                Err(error) => Err(io::Error::other(error)),
            }
        }
        Err(error) => Err(error),
    }
}

struct Pending {
    credentials: Credentials,
    id: String,
    expires: Instant,
}
struct Attempts {
    started: Instant,
    count: u8,
}
impl Attempts {
    fn check(&mut self) -> bool {
        if self.started.elapsed() >= LIMIT_WINDOW {
            self.started = Instant::now();
            self.count = 0;
        }
        if self.count >= MAX_ATTEMPTS {
            return false;
        }
        self.count += 1;
        true
    }
}
struct SetupState {
    attempts: Attempts,
    pending: Option<Pending>,
}
struct Regular {
    gateway: Arc<Gateway>,
    push_worker: Option<tokio::task::JoinHandle<()>>,
}

pub struct BootstrapGateway {
    policy: Arc<Policy>,
    setup_token: String,
    options: Options,
    connector_token: String,
    assets: Assets,
    client: push_transport::Client,
    reporter: Option<Reporter>,
    state: Mutex<SetupState>,
    regular: Mutex<Option<Regular>>,
    stop: CancellationToken,
}
impl BootstrapGateway {
    pub(crate) fn open(
        options: Options,
        connector_token: String,
        assets: Assets,
        client: push_transport::Client,
        reporter: Option<Reporter>,
    ) -> io::Result<Self> {
        clean_path(&options.credentials)?;
        let dir = private_parent(&options.credentials, false)?;
        let name = options
            .credentials
            .file_name()
            .ok_or_else(|| invalid("invalid credentials path"))?;
        match dir.read_private(name, 8192) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "credentials appeared during startup",
                ))
            }
            Err(error) => return Err(error),
        }
        let setup_token = token::load(&token_path(&options.credentials))?;
        if setup_token == connector_token {
            return Err(invalid("setup and connector tokens must be distinct"));
        }
        Ok(Self {
            policy: Arc::new(
                Policy::new(&options.origin, &connector_token)
                    .map_err(|_| invalid("invalid gateway policy"))?,
            ),
            setup_token,
            options,
            connector_token,
            assets,
            client,
            reporter,
            state: Mutex::new(SetupState {
                attempts: Attempts {
                    started: Instant::now(),
                    count: 0,
                },
                pending: None,
            }),
            regular: Mutex::new(None),
            stop: CancellationToken::new(),
        })
    }

    pub(crate) async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let self_for_route = self.clone();
        let result = boundary::serve(
            listener,
            self.policy.clone(),
            move |request, context| {
                let this = self_for_route.clone();
                async move { this.handle(request, context).await }
            },
            shutdown,
        )
        .await;
        self.shutdown().await;
        result
    }
    pub(crate) async fn shutdown(&self) {
        self.stop.cancel();
        if let Some(regular) = self.regular.lock().await.take() {
            regular
                .gateway
                .shutdown_services_with_push(regular.push_worker)
                .await;
        } else {
            self.client.shutdown();
        }
        self.assets.shutdown().await;
    }
    async fn regular(&self) -> io::Result<Arc<Gateway>> {
        let mut guard = self.regular.lock().await;
        if let Some(regular) = guard.as_ref() {
            return Ok(regular.gateway.clone());
        }
        let protected = runtime::asset_boundary(&self.options)?;
        let assets = Assets::open_excluding(&self.options.assets, protected)?;
        let gateway = Arc::new(
            runtime::build_gateway(
                &self.options,
                &self.connector_token,
                assets,
                self.client.clone(),
                self.reporter.clone(),
            )
            .await?,
        );
        let push_worker = gateway.start_push(self.stop.clone());
        *guard = Some(Regular {
            gateway: gateway.clone(),
            push_worker,
        });
        Ok(gateway)
    }
    fn credentials_exist(&self) -> io::Result<bool> {
        let dir = private_parent(&self.options.credentials, false)?;
        let name = self
            .options
            .credentials
            .file_name()
            .ok_or_else(|| invalid("invalid credentials path"))?;
        match dir.read_private(name, 8192) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
    async fn handle(&self, request: Request<Incoming>, context: RequestContext) -> Reply {
        // Drop the setup transition lock before awaiting a normal request.
        // An if-let temporary guard would otherwise serialize every request
        // for the lifetime of this first-login process.
        let regular = {
            let guard = self.regular.lock().await;
            guard.as_ref().map(|regular| regular.gateway.clone())
        };
        if let Some(gateway) = regular {
            return gateway.handle(request, context).await;
        }
        match self.credentials_exist() {
            Ok(true) => {
                return match self.regular().await {
                    Ok(gateway) => gateway.handle(request, context).await,
                    Err(_) => setup_error(StatusCode::SERVICE_UNAVAILABLE),
                }
            }
            Ok(false) => {}
            Err(_) => return setup_error(StatusCode::SERVICE_UNAVAILABLE),
        }
        match (request.uri().path(), request.method()) {
            ("/api/session", &Method::GET) => setup_error(StatusCode::UNAUTHORIZED),
            ("/api/setup/status", &Method::GET) => boundary::json(&json!({"required": true})),
            ("/api/setup/begin", &Method::POST) => self.begin(request).await,
            ("/api/setup/complete", &Method::POST) => self.complete(request).await,
            ("/api/setup/status" | "/api/setup/begin" | "/api/setup/complete", _) => {
                setup_error(StatusCode::METHOD_NOT_ALLOWED)
            }
            (path, _) if !path.starts_with("/api/") && path != "/connect" => {
                self.assets.serve(&request).await
            }
            _ => setup_error(StatusCode::SERVICE_UNAVAILABLE),
        }
    }
    fn valid_token(&self, presented: &str) -> bool {
        presented.len() == self.setup_token.len()
            && bool::from(presented.as_bytes().ct_eq(self.setup_token.as_bytes()))
    }
    async fn begin(&self, request: Request<Incoming>) -> Reply {
        let Ok(body) = boundary::decode_json::<_, BeginBody>(request).await else {
            return setup_error(StatusCode::BAD_REQUEST);
        };
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(_) => return setup_error(StatusCode::TOO_MANY_REQUESTS),
        };
        if !state.attempts.check() {
            return setup_error(StatusCode::TOO_MANY_REQUESTS);
        }
        if !self.valid_token(&body.token) {
            return setup_error(StatusCode::UNAUTHORIZED);
        }
        if body.username.is_empty()
            || body.username.len() > 80
            || body.username.trim() != body.username
            || !(8..=128).contains(&body.password.len())
            || body.password != body.password_confirm
        {
            return setup_error(StatusCode::BAD_REQUEST);
        }
        // The holder may restart an interrupted enrollment. Retain only one
        // challenge: replacing it invalidates the previous enrollment_id.
        state.pending = None;
        let mut salt = [0u8; 32];
        let mut secret = [0u8; 20];
        if getrandom::fill(&mut salt)
            .and_then(|()| getrandom::fill(&mut secret))
            .is_err()
        {
            return setup_error(StatusCode::SERVICE_UNAVAILABLE);
        }
        let password = body.password;
        let hash = match tokio::task::spawn_blocking(move || {
            auth::derive_password(&password, &salt)
        })
        .await
        {
            Ok(hash) => hash,
            Err(_) => return setup_error(StatusCode::SERVICE_UNAVAILABLE),
        };
        let credentials = Credentials {
            username: body.username,
            hash: hash.to_vec(),
            salt: salt.to_vec(),
            totp_secret: data_encoding::BASE32_NOPAD.encode(&secret),
            last_step: 0,
            totp_disabled: !body.totp_enabled,
        };
        if !matches!(self.credentials_exist(), Ok(false)) {
            return setup_error(StatusCode::CONFLICT);
        }
        if !body.totp_enabled {
            return match self.persist(&credentials).await {
                Ok(()) => match self.regular().await {
                    Ok(_) => boundary::json(&json!({"complete": true})),
                    Err(_) => setup_error(StatusCode::SERVICE_UNAVAILABLE),
                },
                Err(error) => persist_error(&error),
            };
        }
        let id = match random_token() {
            Ok(id) => id,
            Err(_) => return setup_error(StatusCode::SERVICE_UNAVAILABLE),
        };
        let uri = totp_uri(&credentials);
        let secret = credentials.totp_secret.clone();
        state.pending = Some(Pending {
            credentials,
            id: id.clone(),
            expires: Instant::now() + PENDING_LIFETIME,
        });
        boundary::json(
            &json!({"complete": false, "enrollment_id": id, "totp_secret": secret, "totp_uri": uri}),
        )
    }
    async fn complete(&self, request: Request<Incoming>) -> Reply {
        let Ok(body) = boundary::decode_json::<_, CompleteBody>(request).await else {
            return setup_error(StatusCode::BAD_REQUEST);
        };
        let mut state = match self.state.try_lock() {
            Ok(state) => state,
            Err(_) => return setup_error(StatusCode::TOO_MANY_REQUESTS),
        };
        if !state.attempts.check() {
            return setup_error(StatusCode::TOO_MANY_REQUESTS);
        }
        if !self.valid_token(&body.token) {
            return setup_error(StatusCode::UNAUTHORIZED);
        }
        let Some(pending) = state.pending.as_mut() else {
            return setup_error(StatusCode::CONFLICT);
        };
        if pending.expires <= Instant::now() {
            state.pending = None;
            return setup_error(StatusCode::CONFLICT);
        }
        if body.enrollment_id.len() != pending.id.len()
            || !bool::from(body.enrollment_id.as_bytes().ct_eq(pending.id.as_bytes()))
        {
            return setup_error(StatusCode::UNAUTHORIZED);
        }
        let Some(step) = pending
            .credentials
            .matches_unused_code(&body.code, chrono::Utc::now().timestamp())
        else {
            return setup_error(StatusCode::UNAUTHORIZED);
        };
        pending.credentials.last_step = step;
        let credentials = pending.credentials.clone();
        if !matches!(self.credentials_exist(), Ok(false)) {
            return setup_error(StatusCode::CONFLICT);
        }
        match self.persist(&credentials).await {
            Ok(()) => {
                state.pending = None;
                drop(state);
                match self.regular().await {
                    Ok(_) => boundary::json(&json!({"complete": true})),
                    Err(_) => setup_error(StatusCode::SERVICE_UNAVAILABLE),
                }
            }
            Err(error) => persist_error(&error),
        }
    }
    async fn persist(&self, credentials: &Credentials) -> io::Result<()> {
        let path = self.options.credentials.clone();
        let encoded = credentials.go_json();
        tokio::task::spawn_blocking(move || {
            let dir = private_parent(&path, false)?;
            let name = path
                .file_name()
                .ok_or_else(|| invalid("invalid credentials path"))?;
            dir.write_new_private(name, encoded.as_bytes())
                .map_err(|error| match error {
                    WriteError::BeforeCommit(error) | WriteError::AfterCommit(error) => error,
                })?;
            let mut setup_name: OsString = name.to_os_string();
            setup_name.push(".bootstrap");
            // Credentials are authoritative even if retiring the setup bearer is interrupted.
            let _ = rustix::fs::unlinkat(&dir, &setup_name, rustix::fs::AtFlags::empty());
            Ok(())
        })
        .await
        .map_err(|_| io::Error::other("credential writer unavailable"))?
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeginBody {
    token: String,
    username: String,
    password: String,
    password_confirm: String,
    totp_enabled: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteBody {
    token: String,
    enrollment_id: String,
    code: String,
}
pub(crate) fn setup_error(status: StatusCode) -> Reply {
    let mut reply = boundary::json(&json!({"error": "setup request failed"}));
    *reply.status_mut() = status;
    reply
}
fn persist_error(error: &io::Error) -> Reply {
    setup_error(if error.kind() == io::ErrorKind::AlreadyExists {
        StatusCode::CONFLICT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    })
}
fn totp_uri(credentials: &Credentials) -> String {
    let mut label = String::new();
    for byte in format!("HMux:{}", credentials.username).bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~:@&=+$".contains(&byte) {
            label.push(byte as char);
        } else {
            use std::fmt::Write;
            write!(&mut label, "%{byte:02X}").expect("write to string");
        }
    }
    format!(
        "otpauth://totp/{label}?algorithm=SHA1&digits=6&issuer=HMux&period=30&secret={}",
        credentials.totp_secret
    )
}
