//! Candidate authentication routes. Unported protected routes fail closed.

use crate::{
    auth_store::{
        AuthStore, LoginRequest, LoginStatus, SecurityRequest, SecurityStatus, StoreError,
    },
    http_boundary::{self as boundary, Policy, Reply, RequestContext},
};
use chrono::{DateTime, Utc};
use http::{Method, Request, StatusCode};
use hyper::body::Incoming;
use serde::Deserialize;
use serde_json::json;
use std::{io, sync::Arc, time::SystemTime};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[path = "http_action.rs"]
mod action;
#[path = "http_push.rs"]
mod push_routes;

pub struct Gateway {
    policy: Arc<Policy>,
    origin: String,
    auth: Arc<AuthStore>,
    home: Option<crate::hub::Hub>,
    preferences: Option<crate::usage_preferences::Store>,
    workspaces: Option<hmux_core::workspace::Store>,
    assets: Option<crate::static_assets::Assets>,
    diagnostics: Option<crate::diagnostics::Store>,
    push: Option<crate::push::Push>,
    uploads: crate::browser_upload::Limiter,
    locations: Option<crate::session_location::Locator>,
}

impl Gateway {
    pub fn new(
        origin: &str,
        connector_token: &str,
        auth: Arc<AuthStore>,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            policy: Arc::new(Policy::new(origin, connector_token)?),
            origin: origin.to_owned(),
            auth,
            home: None,
            preferences: None,
            workspaces: None,
            assets: None,
            diagnostics: None,
            push: None,
            locations: None,
            uploads: crate::browser_upload::Limiter::default(),
        })
    }

    /// Library-only candidate wiring; the auth-only executable does not opt in.
    /// Caller must own and drain the hub's bounded completion receiver.
    pub fn with_home(mut self, hub: crate::hub::Hub) -> Self {
        self.home = Some(hub);
        self
    }

    pub fn with_preferences(mut self, store: crate::usage_preferences::Store) -> Self {
        self.preferences = Some(store);
        self
    }

    pub fn with_workspaces(mut self, store: hmux_core::workspace::Store) -> Self {
        self.workspaces = Some(store);
        self
    }
    pub fn with_assets(mut self, assets: crate::static_assets::Assets) -> Self {
        self.assets = Some(assets);
        self
    }

    pub fn with_diagnostics(mut self, store: crate::diagnostics::Store) -> Self {
        self.diagnostics = Some(store);
        self
    }

    /// Opt in to the single gateway-owned push store, HTTPS client and the
    /// Hub's existing bounded completion receiver.
    pub fn with_push(mut self, push: crate::push::Push) -> Self {
        self.push = Some(push);
        self
    }

    pub fn with_locations(mut self, locations: crate::session_location::Locator) -> Self {
        self.locations = Some(locations);
        self
    }

    pub async fn serve(
        self: Arc<Self>,
        listener: TcpListener,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let push_worker = self.push.as_ref().and_then(|push| {
            self.home.as_ref().and_then(|home| {
                push.start(
                    self.auth.clone(),
                    home.clone(),
                    self.workspaces.clone(),
                    self.origin.clone(),
                    shutdown.clone(),
                )
            })
        });
        let gateway = self.clone();
        let result = boundary::serve(
            listener,
            self.policy.clone(),
            move |request, context| {
                let gateway = gateway.clone();
                async move { gateway.handle(request, context).await }
            },
            shutdown,
        )
        .await;
        self.shutdown_services_with_push(push_worker).await;
        result
    }

    pub(crate) async fn shutdown_services(&self) {
        self.shutdown_services_with_push(None).await;
    }
    async fn shutdown_services_with_push(&self, push_worker: Option<tokio::task::JoinHandle<()>>) {
        if let Some(locations) = &self.locations {
            locations.shutdown();
        }
        if let Some(push) = &self.push {
            push.shutdown(push_worker).await;
        }
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.shutdown().await;
        }
        if let Some(assets) = &self.assets {
            assets.shutdown().await;
        }
        if let Some(workspaces) = &self.workspaces {
            workspaces.shutdown().await;
        }
        if let Some(preferences) = &self.preferences {
            preferences.shutdown().await;
        }
        self.auth.shutdown().await;
    }

    async fn handle(&self, request: Request<Incoming>, context: RequestContext) -> Reply {
        if request.uri().path() == "/connect" {
            return match &self.home {
                Some(hub) => crate::http_home::connect(&self.policy, hub, request, context),
                None => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
            };
        }
        if request.uri().path() == "/api/login" {
            return self.login(request, context).await;
        }
        if !request.uri().path().starts_with("/api/") {
            return match &self.assets {
                Some(assets) => assets.serve(&request).await,
                None => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
            };
        }
        let Some(token) = boundary::session_token(request.headers()).map(str::to_owned) else {
            return boundary::error(StatusCode::UNAUTHORIZED);
        };
        let access = match self.auth.access(&token, false, now()).await {
            Ok(Some(access)) => access,
            Ok(None) => return boundary::error(StatusCode::UNAUTHORIZED),
            Err(_) => return boundary::error(StatusCode::SERVICE_UNAVAILABLE),
        };
        if request.method() != Method::GET && !boundary::valid_csrf(request.headers(), &access.csrf)
        {
            return boundary::error(StatusCode::FORBIDDEN);
        }
        let method = request.method().clone();
        match request.uri().path() {
            "/api/terminal" if method == Method::GET => {
                self.terminal(request, context, token, access)
            }
            "/api/upload" if method == Method::GET => self.upload(request, context, token, access),
            "/api/session" if method == Method::GET => boundary::json(&json!({
                "username": access.username, "profile": access.profile,
                "csrf": access.csrf, "login_id": access.id,
            })),
            "/api/logout" if method == Method::POST => {
                if self.auth.logout(&token).await.is_err() {
                    return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
                }
                let mut reply = boundary::json(&json!({"ok": true}));
                let _ = boundary::set_cookie(&mut reply, None);
                reply
            }
            "/api/sessions" if method == Method::GET => {
                match self.auth.list_sessions_checked(&token, now()).await {
                    Ok(Some(mut sessions)) => {
                        if let Some(locations) = &self.locations {
                            let mut current = access.clone();
                            tokio::select! {
                                biased;
                                _ = current.wait_cancelled() => return boundary::error(StatusCode::UNAUTHORIZED),
                                _ = locations.enrich(&mut sessions) => {},
                            }
                        }
                        if let Err(status) = self.revalidate(&token).await {
                            return boundary::error(status);
                        }
                        boundary::json(&json!({"sessions": sessions}))
                    }
                    Ok(None) => boundary::error(StatusCode::UNAUTHORIZED),
                    Err(_) => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
                }
            }
            "/api/sessions/revoke" if method == Method::POST => {
                let Ok(body) = boundary::decode_json::<_, RevokeBody>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                match self.auth.revoke(&token, &body.id, now()).await {
                    Ok(current) => {
                        let mut reply = boundary::json(&json!({"ok": true}));
                        if current {
                            let _ = boundary::set_cookie(&mut reply, None);
                        }
                        reply
                    }
                    Err(StoreError::SessionNotFound) => boundary::error(StatusCode::NOT_FOUND),
                    Err(_) => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
                }
            }
            "/api/account/security" => self.security(request, &token).await,
            "/api/account/usage" => self.usage_preferences(request, &token, &access).await,
            "/api/state" if method == Method::GET => self.state(&token, &access).await,
            "/api/state" => boundary::error(StatusCode::METHOD_NOT_ALLOWED),
            "/api/action" => self.action(request, &token).await,
            "/api/diagnostics" => self.diagnostics(request, &token, &access).await,
            "/api/session" | "/api/logout" | "/api/sessions" | "/api/sessions/revoke" => {
                boundary::error(StatusCode::METHOD_NOT_ALLOWED)
            }
            "/api/push"
            | "/api/push/subscribe"
            | "/api/push/unsubscribe"
            | "/api/push/presence"
            | "/api/push/test" => self.push_api(request, &token, &access).await,
            "/api/terminal" | "/api/upload" => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
            _ => boundary::error(StatusCode::NOT_FOUND),
        }
    }

    async fn diagnostics(
        &self,
        request: Request<Incoming>,
        token: &str,
        access: &crate::auth_store::SessionAccess,
    ) -> Reply {
        use crate::diagnostics::{Batch, Error};
        let Some(store) = &self.diagnostics else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let map_error = |error| {
            boundary::error(match error {
                Error::Invalid => StatusCode::BAD_REQUEST,
                Error::Unauthorized => StatusCode::UNAUTHORIZED,
                Error::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                Error::Busy | Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            })
        };
        match *request.method() {
            Method::GET => {
                let report = match store.report(access) {
                    Ok(report) => report,
                    Err(error) => return map_error(error),
                };
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                boundary::json(&report)
            }
            Method::POST => {
                let browser = boundary::browser_label(
                    request
                        .headers()
                        .get(http::header::USER_AGENT)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default(),
                );
                let Ok(batch) = boundary::decode_json::<_, Batch>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                let status = match store.append(access, &browser, batch) {
                    Ok(()) => StatusCode::ACCEPTED,
                    Err(Error::RateLimited) => StatusCode::TOO_MANY_REQUESTS,
                    Err(error) => return map_error(error),
                };
                let mut reply = http::Response::new(boundary::Body::new(bytes::Bytes::new()));
                *reply.status_mut() = status;
                if status == StatusCode::TOO_MANY_REQUESTS {
                    reply.headers_mut().insert(
                        http::header::RETRY_AFTER,
                        http::HeaderValue::from_static("60"),
                    );
                }
                boundary::secure_headers(&mut reply);
                reply
            }
            _ => boundary::error(StatusCode::METHOD_NOT_ALLOWED),
        }
    }

    async fn usage_preferences(
        &self,
        request: Request<Incoming>,
        token: &str,
        access: &crate::auth_store::SessionAccess,
    ) -> Reply {
        use crate::usage_preferences::{Error, Preferences};
        let Some(store) = &self.preferences else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let result = match *request.method() {
            Method::GET => store.get(access).await,
            Method::POST => {
                let Ok(next) = boundary::decode_json::<_, Preferences>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                if !next.valid() {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                // Slow request bodies must not let an expired/revoked session
                // start a new write. A begun atomic transaction may complete.
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                store.set(access, next).await
            }
            _ => return boundary::error(StatusCode::METHOD_NOT_ALLOWED),
        };
        let value = match result {
            Ok(value) => value,
            Err(Error::Invalid) => return boundary::error(StatusCode::BAD_REQUEST),
            Err(Error::Conflict) => return boundary::error(StatusCode::CONFLICT),
            Err(Error::Unauthorized) => return boundary::error(StatusCode::UNAUTHORIZED),
            Err(_) => return boundary::error(StatusCode::SERVICE_UNAVAILABLE),
        };
        if let Err(status) = self.revalidate(token).await {
            return boundary::error(status);
        }
        boundary::json(&value)
    }

    async fn revalidate(&self, token: &str) -> Result<(), StatusCode> {
        match self.auth.access(token, false, now()).await {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(StatusCode::UNAUTHORIZED),
            Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    }

    async fn state(&self, token: &str, access: &crate::auth_store::SessionAccess) -> Reply {
        use serde::Serialize;
        use serde_json::value::RawValue;
        #[derive(Serialize)]
        struct Usage<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            claude: Option<&'a RawValue>,
            #[serde(skip_serializing_if = "Option::is_none")]
            codex: Option<&'a RawValue>,
        }
        #[derive(Serialize)]
        struct State<'a> {
            online: bool,
            catalog: Option<&'a RawValue>,
            usage: Usage<'a>,
            #[serde(skip_serializing_if = "Option::is_none")]
            usage_preferences: Option<crate::usage_preferences::Preferences>,
        }
        fn raw(value: &Option<bytes::Bytes>) -> Result<Option<&RawValue>, serde_json::Error> {
            value
                .as_ref()
                .map(|v| serde_json::from_slice(v))
                .transpose()
        }
        let Some(home) = &self.home else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let snapshot = home.snapshot();
        let (Ok(catalog), Ok(claude), Ok(codex)) = (
            raw(&snapshot.catalog),
            raw(&snapshot.claude_usage),
            raw(&snapshot.codex_usage),
        ) else {
            return boundary::error(StatusCode::BAD_GATEWAY);
        };
        // Match Go: temporarily unavailable preferences are omitted; cached Home
        // state remains available. No DTO tree is allocated for retained JSON.
        let usage_preferences = match &self.preferences {
            Some(store) => store.get(access).await.ok(),
            None => None,
        };
        if let Err(status) = self.revalidate(token).await {
            return boundary::error(status);
        }
        boundary::json(&State {
            online: snapshot.online,
            catalog,
            usage: Usage { claude, codex },
            usage_preferences,
        })
    }

    fn terminal(
        &self,
        mut request: Request<Incoming>,
        context: RequestContext,
        token: String,
        access: crate::auth_store::SessionAccess,
    ) -> Reply {
        let Some(hub) = self.home.clone() else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let response = match crate::http_upgrade::terminal_handshake(&self.policy, &request) {
            Ok(response) => response,
            Err(status) => return boundary::error(status),
        };
        let auth = self.auth.clone();
        if context
            .spawn_upgrade_graceful(&mut request, move |socket, shutdown| async move {
                let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    socket,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    Some(crate::browser_terminal::socket_config()),
                )
                .await;
                let Ok(connection) = hmux_protocol::transport::start_frames(
                    socket,
                    crate::browser_terminal::FRAME_BYTES,
                ) else {
                    return;
                };
                crate::browser_terminal::serve(connection, hub, auth, token, access, shutdown)
                    .await;
            })
            .is_err()
        {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        }
        response
    }

    fn upload(
        &self,
        mut request: Request<Incoming>,
        context: RequestContext,
        token: String,
        access: crate::auth_store::SessionAccess,
    ) -> Reply {
        let Some(hub) = self.home.clone() else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let response = match crate::http_upgrade::upload_handshake(&self.policy, &request) {
            Ok(response) => response,
            Err(status) => return boundary::error(status),
        };
        let auth = self.auth.clone();
        let limiter = self.uploads.clone();
        if context
            .spawn_upgrade_graceful(&mut request, move |socket, shutdown| async move {
                crate::browser_upload::serve(socket, hub, auth, token, access, limiter, shutdown)
                    .await;
            })
            .is_err()
        {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        }
        response
    }

    async fn login(&self, request: Request<Incoming>, context: RequestContext) -> Reply {
        if request.method() != Method::POST {
            return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
        }
        let ip = boundary::login_ip(context.peer, request.headers());
        let browser = boundary::browser_label(
            request
                .headers()
                .get(http::header::USER_AGENT)
                .and_then(|h| h.to_str().ok())
                .unwrap_or(""),
        );
        let Ok(body) = boundary::decode_json::<_, LoginBody>(request).await else {
            return boundary::error(StatusCode::BAD_REQUEST);
        };
        let result = self
            .auth
            .login(LoginRequest {
                username: body.username,
                password: body.password,
                code: body.code,
                source: boundary::login_source(ip),
                ip: ip.to_string(),
                browser,
                now: now(),
            })
            .await;
        match result.status {
            LoginStatus::TotpRequired => boundary::json(&json!({"totp_required": true})),
            LoginStatus::Succeeded => {
                let mut reply = boundary::json(&json!({"ok": true}));
                match result.token.as_deref() {
                    Some(token) if boundary::set_cookie(&mut reply, Some(token)).is_ok() => reply,
                    _ => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
                }
            }
            // Preserve the generic Go failure contract, including admission and
            // storage failures, without disclosing whether an account exists.
            _ => boundary::error(StatusCode::UNAUTHORIZED),
        }
    }

    async fn security(&self, request: Request<Incoming>, token: &str) -> Reply {
        match *request.method() {
            Method::GET => match self.auth.totp_enabled_checked(token, now()).await {
                Ok(Some(enabled)) => boundary::json(&json!({"totp_enabled": enabled})),
                Ok(None) => boundary::error(StatusCode::UNAUTHORIZED),
                Err(_) => boundary::error(StatusCode::SERVICE_UNAVAILABLE),
            },
            Method::POST => {
                let Ok(body) = boundary::decode_json::<_, SecurityBody>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                let Some(enabled) = body.totp_enabled else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                match self
                    .auth
                    .set_totp_enabled(SecurityRequest {
                        token: token.to_owned(),
                        password: body.password,
                        code: body.code,
                        enabled,
                        now: now(),
                    })
                    .await
                {
                    SecurityStatus::Ok => boundary::json(&json!({"totp_enabled": enabled})),
                    SecurityStatus::Forbidden => boundary::error(StatusCode::FORBIDDEN),
                    SecurityStatus::RateLimited => boundary::error(StatusCode::TOO_MANY_REQUESTS),
                    SecurityStatus::StorageUnavailable => {
                        boundary::error(StatusCode::SERVICE_UNAVAILABLE)
                    }
                    SecurityStatus::StaleSession => boundary::error(StatusCode::UNAUTHORIZED),
                }
            }
            _ => boundary::error(StatusCode::METHOD_NOT_ALLOWED),
        }
    }
}

fn now() -> DateTime<Utc> {
    SystemTime::now().into()
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LoginBody {
    username: String,
    password: String,
    code: String,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RevokeBody {
    id: String,
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SecurityBody {
    totp_enabled: Option<bool>,
    password: String,
    code: String,
}
