//! Account-scoped, compare-and-set completion read receipts.
use super::*;
use crate::tab_completions::{self, Error, ReadRequest};

impl Gateway {
    pub(super) async fn read_completion(&self, request: Request<Incoming>, token: &str) -> Reply {
        if request.method() != Method::POST {
            return boundary::error(StatusCode::METHOD_NOT_ALLOWED);
        }
        let Ok(request) = boundary::decode_json::<_, ReadRequest>(request).await else {
            return boundary::error(StatusCode::BAD_REQUEST);
        };
        let access = match self.auth.access(token, true, now()).await {
            Ok(Some(access)) => access,
            Ok(None) => return boundary::error(StatusCode::UNAUTHORIZED),
            Err(_) => return boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE),
        };
        let (Some(home), Some(store)) = (&self.home, &self.completions) else {
            return boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE);
        };
        let snapshot = home.snapshot();
        if !snapshot.online {
            return boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE);
        }
        let live = tab_completions::live_sessions(snapshot.catalog.as_deref());
        let expected = request.session.clone();
        let hub = home.clone();
        let generation = snapshot.generation;
        let result = store
            .acknowledge(&access, request, live, move || {
                let current = hub.snapshot();
                current.online
                    && current.generation == generation
                    && tab_completions::live_sessions(current.catalog.as_deref())
                        .contains(&expected)
            })
            .await;
        if let Err(status) = self.revalidate(token).await {
            return boundary::error(status);
        }
        match result {
            Ok(snapshot) => boundary::json(&snapshot),
            Err(Error::Invalid) => boundary::error(StatusCode::BAD_REQUEST),
            Err(Error::Conflict) => boundary::error(StatusCode::CONFLICT),
            Err(Error::Unauthorized) => boundary::error(StatusCode::UNAUTHORIZED),
            Err(Error::Busy | Error::Unavailable) => {
                boundary::retryable_error(StatusCode::SERVICE_UNAVAILABLE)
            }
        }
    }
}
