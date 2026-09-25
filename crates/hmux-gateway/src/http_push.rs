//! Browser push routes. The cookie/CSRF boundary is enforced by Gateway before
//! entering here; request bodies contain no account or login selector.
use super::*;
use crate::{
    push,
    push_state::{self, Keys, Subscription},
    push_transport,
};
use hmux_model::SessionIdentity;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubscribeBody {
    endpoint: String,
    keys: Keys,
    #[serde(rename = "expirationTime")]
    expiration_time: Option<f64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyBody {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceBody {
    client_id: String,
    session: Option<PresenceSession>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresenceSession {
    id: String,
    created_at: i64,
}
fn store_status(error: push_state::Error) -> StatusCode {
    match error {
        push_state::Error::Invalid => StatusCode::BAD_REQUEST,
        push_state::Error::Unauthorized => StatusCode::UNAUTHORIZED,
        push_state::Error::Busy | push_state::Error::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}
fn delivery_failure(result: Result<StatusCode, push_transport::Error>) -> Option<StatusCode> {
    match result {
        Ok(status) if status.is_success() => None,
        Ok(_) | Err(push_transport::Error::Invalid | push_transport::Error::Redirect) => {
            Some(StatusCode::BAD_GATEWAY)
        }
        Err(push_transport::Error::Unauthorized) => Some(StatusCode::UNAUTHORIZED),
        Err(push_transport::Error::Timeout) => Some(StatusCode::GATEWAY_TIMEOUT),
        Err(
            push_transport::Error::Busy
            | push_transport::Error::Cancelled
            | push_transport::Error::Unavailable,
        ) => Some(StatusCode::SERVICE_UNAVAILABLE),
    }
}
impl Gateway {
    pub(super) async fn push_api(
        &self,
        request: Request<Incoming>,
        token: &str,
        access: &crate::auth_store::SessionAccess,
    ) -> Reply {
        let Some(push) = &self.push else {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        };
        if self.home.is_none() {
            return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
        }
        let path = request.uri().path().to_owned();
        if path == "/api/push" {
            if request.method() != Method::GET {
                return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
            }
            let config = match push.store().public_config(access).await {
                Ok(config) => config,
                Err(error) => return boundary::error(store_status(error)),
            };
            if let Err(status) = self.revalidate(token).await {
                return boundary::error(status);
            }
            return boundary::json(&config);
        }
        if request.method() != Method::POST {
            return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
        }
        match path.as_str() {
            "/api/push/subscribe" => {
                let Ok(body) = boundary::decode_json::<_, SubscribeBody>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                if body.expiration_time.is_some_and(|v| !v.is_finite()) {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                let sub = Subscription {
                    endpoint: body.endpoint,
                    keys: body.keys,
                };
                if push_state::validate_subscription(&sub).is_err() {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                let fresh = match self.auth.access(token, false, now()).await {
                    Ok(Some(fresh)) if fresh.id == access.id => fresh,
                    Ok(_) => return boundary::error(StatusCode::UNAUTHORIZED),
                    Err(_) => return boundary::error(StatusCode::SERVICE_UNAVAILABLE),
                };
                let auth = self.auth.clone();
                let result = push
                    .store()
                    .subscribe(&fresh, sub, move |id| {
                        auth.push_login_by_id(id, now())
                            .map(|found| found.is_some())
                            .map_err(|error| match error {
                                StoreError::Busy => push_state::Error::Busy,
                                _ => push_state::Error::Unavailable,
                            })
                    })
                    .await;
                if let Err(error) = result {
                    return boundary::error(store_status(error));
                }
                push.prune_transient().await;
            }
            "/api/push/unsubscribe" => {
                if boundary::decode_json::<_, EmptyBody>(request)
                    .await
                    .is_err()
                {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                if let Err(error) = push.store().remove(&access.id, None).await {
                    return boundary::error(store_status(error));
                }
                push.forget(&access.id);
            }
            "/api/push/presence" => {
                let Ok(body) = boundary::decode_json::<_, PresenceBody>(request).await else {
                    return boundary::error(StatusCode::BAD_REQUEST);
                };
                if body.client_id.is_empty()
                    || body.client_id.len() > 64
                    || body
                        .client_id
                        .chars()
                        .any(|c| c <= '\u{1f}' || c == '\u{7f}')
                    || body.session.as_ref().is_some_and(|s| {
                        hmux_model::validate_session_id(&s.id).is_err() || s.created_at < 1
                    })
                {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                let session = body.session.map(|s| SessionIdentity {
                    id: s.id,
                    created_at: s.created_at,
                });
                push.presence(&access.id, &body.client_id, session);
            }
            "/api/push/test" => {
                if boundary::decode_json::<_, EmptyBody>(request)
                    .await
                    .is_err()
                {
                    return boundary::error(StatusCode::BAD_REQUEST);
                }
                if let Err(status) = self.revalidate(token).await {
                    return boundary::error(status);
                }
                let sub = match push.subscription(&access.id).await {
                    Ok(Some(sub)) => sub,
                    Ok(None) => return boundary::error(StatusCode::CONFLICT),
                    Err(error) => return boundary::error(store_status(error)),
                };
                if !push.test_admit(&access.id) {
                    return boundary::error(StatusCode::TOO_MANY_REQUESTS);
                }
                let Some(home) = &self.home else {
                    return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
                };
                let Some(event_id) = push::test_event_id() else {
                    return boundary::error(StatusCode::SERVICE_UNAVAILABLE);
                };
                let payload = json!({"type":"test", "login_id":access.id, "event_id":event_id});
                let context = push::DeliveryContext {
                    auth: self.auth.clone(),
                    home: home.clone(),
                    workspaces: self.workspaces.clone(),
                    origin: self.origin.clone(),
                };
                let delivery = push
                    .send(&context, access.clone(), sub, payload, None, None)
                    .await;
                if let Some(status) = delivery_failure(delivery) {
                    return if matches!(
                        status,
                        StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
                    ) {
                        boundary::retryable_error(status)
                    } else {
                        boundary::error(status)
                    };
                }
            }
            _ => return boundary::error(StatusCode::NOT_FOUND),
        }
        boundary::json(&json!({"ok":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_delivery_distinguishes_local_limits_from_remote_failure() {
        assert_eq!(delivery_failure(Ok(StatusCode::CREATED)), None);
        assert_eq!(
            delivery_failure(Ok(StatusCode::GONE)),
            Some(StatusCode::BAD_GATEWAY)
        );
        for error in [
            push_transport::Error::Busy,
            push_transport::Error::Unavailable,
            push_transport::Error::Cancelled,
        ] {
            assert_eq!(
                delivery_failure(Err(error)),
                Some(StatusCode::SERVICE_UNAVAILABLE)
            );
        }
        assert_eq!(
            delivery_failure(Err(push_transport::Error::Timeout)),
            Some(StatusCode::GATEWAY_TIMEOUT)
        );
        assert_eq!(
            delivery_failure(Err(push_transport::Error::Invalid)),
            Some(StatusCode::BAD_GATEWAY)
        );
    }

    #[test]
    fn browser_subscription_dto_accepts_nullable_expiration_and_rejects_selectors() {
        let valid = r#"{"endpoint":"https://fcm.googleapis.com/x","keys":{"auth":"a","p256dh":"b"},"expirationTime":null}"#;
        assert!(serde_json::from_str::<SubscribeBody>(valid).is_ok());
        let number = valid.replace("null", "1234.5");
        assert_eq!(
            serde_json::from_str::<SubscribeBody>(&number)
                .unwrap()
                .expiration_time,
            Some(1234.5)
        );
        for raw in [
            valid.replace("null", "\"tomorrow\""),
            valid.replace("null", "{}"),
            valid.replace("null", "null,\"expirationTime\":5"),
            valid.replace("null", "null,\"login_id\":\"foreign\""),
            valid.replace("null", "1e999"),
        ] {
            assert!(
                serde_json::from_str::<SubscribeBody>(&raw).is_err(),
                "{raw}"
            );
        }
        assert!(serde_json::from_str::<EmptyBody>(r#"{"login_id":"foreign"}"#).is_err());
        assert!(serde_json::from_str::<PresenceBody>(
            r#"{"client_id":"c","session":null,"session":null}"#
        )
        .is_err());
        assert!(serde_json::from_str::<PresenceBody>(
            r#"{"client_id":"c","session":{"id":"$1","created_at":1,"login_id":"foreign"}}"#
        )
        .is_err());
    }
}
