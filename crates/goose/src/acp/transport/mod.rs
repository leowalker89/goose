pub mod auth;
pub mod connection;
pub mod http;
pub mod websocket;

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{
        ws::{rejection::WebSocketUpgradeRejection, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderName, Method, Request},
    response::Response,
    routing::{delete, get, post},
    Router,
};
use serde_json::Value;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::acp::server_factory::AcpServer;

pub(crate) const HEADER_CONNECTION_ID: &str = "Acp-Connection-Id";
pub(crate) const HEADER_SESSION_ID: &str = "Acp-Session-Id";
pub(crate) const EVENT_STREAM_MIME_TYPE: &str = "text/event-stream";
pub(crate) const JSON_MIME_TYPE: &str = "application/json";

pub(crate) fn accepts_mime_type(request: &Request<Body>, mime_type: &str) -> bool {
    request
        .headers()
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|accept| accept.contains(mime_type))
}

pub(crate) fn content_type_is_json(request: &Request<Body>) -> bool {
    request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with(JSON_MIME_TYPE))
}

pub(crate) fn header_value(request: &Request<Body>, name: &str) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

pub(crate) fn is_jsonrpc_request_with_id(value: &Value) -> bool {
    value.get("method").is_some() && value.get("id").is_some()
}

pub(crate) fn is_jsonrpc_notification(value: &Value) -> bool {
    value.get("method").is_some() && value.get("id").is_none()
}

pub(crate) fn is_jsonrpc_response(value: &Value) -> bool {
    value.get("id").is_some()
        && value.get("method").is_none()
        && (value.get("result").is_some() || value.get("error").is_some())
}

pub(crate) fn is_initialize_request(value: &Value) -> bool {
    value.get("method").is_some_and(|m| m == "initialize") && value.get("id").is_some()
}

/// Methods that are scoped to a session and require an Acp-Session-Id header.
pub(crate) fn method_requires_session_header(method: &str) -> bool {
    matches!(
        method,
        "session/prompt"
            | "session/cancel"
            | "session/load"
            | "session/set_mode"
            | "session/set_model"
    )
}

async fn handle_get(
    ws_upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    State(state): State<Arc<connection::ConnectionRegistry>>,
    request: Request<Body>,
) -> Response {
    match ws_upgrade {
        Ok(ws) => websocket::handle_ws_upgrade(state, ws).await,
        Err(_) => http::handle_get(state, request).await,
    }
}

async fn health() -> &'static str {
    "ok"
}

/// Returns true for origins that legitimate local ACP clients use.
///
/// The ACP endpoint is served on loopback and consumed by native clients
/// (the Electron desktop renderer sends `Origin: null` for `file://` pages,
/// local tooling sends no `Origin` at all). Arbitrary web origins must be
/// rejected so a malicious page the victim visits cannot drive the agent
/// (browser CSRF -> RCE via the default `developer` builtin). See CWE-942.
fn is_allowed_acp_origin(origin: &header::HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };

    if origin == "null" {
        return true;
    }

    let Some(host) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = host.split('/').next().unwrap_or(host);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let host = host.trim_start_matches('[').trim_end_matches(']');

    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn acp_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _request_parts| {
            is_allowed_acp_origin(origin)
        }))
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("x-secret-key"),
            HeaderName::from_static("acp-connection-id"),
            HeaderName::from_static("acp-session-id"),
            header::SEC_WEBSOCKET_VERSION,
            header::SEC_WEBSOCKET_KEY,
            header::CONNECTION,
            header::UPGRADE,
        ])
        .expose_headers([
            HeaderName::from_static("acp-connection-id"),
            HeaderName::from_static("acp-session-id"),
        ])
}

fn create_acp_routes(server: Arc<AcpServer>) -> Router {
    let registry = Arc::new(connection::ConnectionRegistry::new(server));

    Router::new()
        .route("/acp", post(http::handle_post).with_state(registry.clone()))
        .route("/acp", get(handle_get).with_state(registry.clone()))
        .route("/acp", delete(http::handle_delete).with_state(registry))
}

pub fn create_acp_router(server: Arc<AcpServer>) -> Router {
    create_acp_routes(server).layer(acp_cors_layer())
}

pub fn create_router(server: Arc<AcpServer>, secret_key: String, require_token: bool) -> Router {
    let mut acp_routes = create_acp_routes(server);
    if require_token {
        acp_routes = acp_routes.layer(axum::middleware::from_fn_with_state(
            secret_key.clone(),
            auth::check_acp_token,
        ));
    }
    acp_routes
        .route("/health", get(health))
        .route("/status", get(health))
        .merge(super::mcp_app_proxy::routes(secret_key))
        .layer(acp_cors_layer())
}

#[cfg(test)]
mod cors_tests {
    use super::*;
    use axum::http::HeaderValue;

    fn origin(value: &str) -> HeaderValue {
        HeaderValue::from_str(value).unwrap()
    }

    #[test]
    fn rejects_arbitrary_web_origins() {
        assert!(!is_allowed_acp_origin(&origin("https://evil.example")));
        assert!(!is_allowed_acp_origin(&origin("http://attacker.com")));
        assert!(!is_allowed_acp_origin(&origin(
            "https://localhost.evil.example"
        )));
        assert!(!is_allowed_acp_origin(&origin("http://127.0.0.1.evil.com")));
    }

    #[test]
    fn allows_local_and_null_origins() {
        assert!(is_allowed_acp_origin(&origin("null")));
        assert!(is_allowed_acp_origin(&origin("http://localhost")));
        assert!(is_allowed_acp_origin(&origin("http://localhost:3284")));
        assert!(is_allowed_acp_origin(&origin("http://127.0.0.1:3284")));
        assert!(is_allowed_acp_origin(&origin("https://127.0.0.1")));
        assert!(is_allowed_acp_origin(&origin("http://[::1]:3284")));
    }

    // `/acp` is unauthenticated and can spawn shells via the default developer
    // builtin; the CORS layer must not let any website read its responses
    // (browser CSRF -> RCE). See CWE-942.
    #[tokio::test]
    async fn acp_cors_does_not_allow_arbitrary_web_origins() {
        let app = Router::new()
            .route("/acp", post(|| async { "ok" }))
            .layer(acp_cors_layer());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let response = reqwest::Client::new()
            .request(reqwest::Method::OPTIONS, format!("http://{addr}/acp"))
            .header("Origin", "https://evil.example")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "content-type,acp-connection-id",
            )
            .send()
            .await
            .unwrap();

        let allow_origin = response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());

        assert_ne!(
            allow_origin.as_deref(),
            Some("*"),
            "any web origin can read responses from the unauthenticated ACP agent server"
        );
        assert_ne!(
            allow_origin.as_deref(),
            Some("https://evil.example"),
            "ACP CORS must not reflect an arbitrary web origin"
        );
    }
}
