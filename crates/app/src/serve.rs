//! The serve shell: axum, one route, a bearer token, and the UI embedded from `ui/dist`.
//! Design section 9 lists the measures that belong to this layer.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Request as HttpRequest, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use cm_core::{ApiError, Request, dispatch};
use cm_host::Host;
use rust_embed::RustEmbed;
use subtle::ConstantTimeEq;

/// Request bodies over this size get 413.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

const CSP: &str = "default-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";

#[derive(RustEmbed)]
#[folder = "../../ui/dist/"]
struct Assets;

#[derive(Clone)]
struct AppState {
    host: Arc<Host>,
    token: Arc<str>,
    /// The `Host` header values a loopback bind accepts. Empty for any other bind.
    loopback_hosts: Arc<[String]>,
}

/// `CONTEXT_MANAGER_TOKEN` when set and non-empty, else 32 random bytes as hex.
pub fn token(pinned: Option<String>) -> String {
    if let Some(pinned) = pinned.filter(|t| !t.is_empty()) {
        return pinned;
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the operating system provides random bytes");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn router(host: Arc<Host>, token: String, bound: SocketAddr) -> Router {
    let loopback_hosts: Vec<String> = if bound.ip().is_loopback() {
        let port = bound.port();
        vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ]
    } else {
        Vec::new()
    };
    let state = AppState {
        host,
        token: token.into(),
        loopback_hosts: loopback_hosts.into(),
    };
    Router::new()
        .route("/api/rpc", post(rpc))
        .fallback_service(get(asset))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

pub async fn run(
    host: Arc<Host>,
    bind_host: &str,
    port: u16,
    token: String,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind((bind_host, port)).await?;
    let bound = listener.local_addr()?;
    if !bound.ip().is_loopback() {
        eprintln!(
            "warning: {bound} is not a loopback address. Anyone who can reach it and holds the token can run commands on this machine.\n\
             Prefer a loopback bind and an SSH tunnel: ssh -L {port}:127.0.0.1:{port} <this-host>",
            port = bound.port()
        );
    }
    let shown = match bound.ip() {
        std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        ip => ip.to_string(),
    };
    eprintln!("http://{shown}:{}/#token={token}", bound.port());
    let app = router(host, token, bound);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiError::Forbidden {
            why: cm_core::RuleRef::new("serve.origin"),
        }),
    )
        .into_response()
}

/// Host and Origin checks, then the token for `/api/*`, then the security headers on whatever
/// comes back.
async fn guard(State(state): State<AppState>, request: HttpRequest, next: Next) -> Response {
    let headers = request.headers();
    let host_header = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // DNS rebinding: on a loopback bind the Host must be a loopback name with the bound port.
    if !state.loopback_hosts.is_empty()
        && !state
            .loopback_hosts
            .iter()
            .any(|h| h.eq_ignore_ascii_case(host_header))
    {
        return with_headers(forbidden());
    }
    // A page in the operator's browser forging a request: the Origin must be this very host.
    if let Some(origin) = headers.get(header::ORIGIN) {
        let authority = origin
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://"))
            .map(|(_, rest)| rest);
        if authority != Some(host_header) {
            return with_headers(forbidden());
        }
    }
    if request.uri().path().starts_with("/api/") && !bearer_matches(request.headers(), &state.token)
    {
        return with_headers(
            (StatusCode::UNAUTHORIZED, Json(ApiError::Unauthorized)).into_response(),
        );
    }
    with_headers(next.run(request).await)
}

fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    // Compared in constant time. An empty token never matches.
    !given.is_empty() && bool::from(given.as_bytes().ct_eq(token.as_bytes()))
}

fn with_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn status_of(error: &ApiError) -> StatusCode {
    match error {
        ApiError::NotFound => StatusCode::NOT_FOUND,
        ApiError::Conflict { .. } => StatusCode::CONFLICT,
        ApiError::OutsideBoundary { .. }
        | ApiError::ReadOnly { .. }
        | ApiError::Forbidden { .. } => StatusCode::FORBIDDEN,
        ApiError::InvalidEdit { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        ApiError::Io { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
    }
}

async fn rpc(State(state): State<AppState>, body: Bytes) -> Response {
    let request: Request = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(e) => {
            let error = ApiError::Io {
                detail: format!("bad request: {e}"),
            };
            return (StatusCode::BAD_REQUEST, Json(error)).into_response();
        }
    };
    match dispatch(state.host.as_ref(), request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => (status_of(&error), Json(error)).into_response(),
    }
}

/// The embedded UI. A path without an extension is a client-side route and gets `index.html`.
async fn asset(method: Method, uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let found = Assets::get(path).or_else(|| {
        (!path.contains('.'))
            .then(|| Assets::get("index.html"))
            .flatten()
    });
    match found {
        Some(file) => {
            let mime = file.metadata.mimetype().to_owned();
            let cache = if path == "index.html" || !path.contains('.') {
                "no-cache"
            } else {
                "public, max-age=31536000, immutable"
            };
            let body = if method == Method::HEAD {
                Vec::new()
            } else {
                file.data.into_owned()
            };
            (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, cache.to_owned()),
                ],
                body,
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cm_core::testing::Fixture;
    use cm_core::{Shell, Snapshot};
    use cm_host::HostConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TOKEN: &str = "test-token";

    struct Reply {
        status: u16,
        headers: String,
        body: String,
    }

    async fn start(fx: &Fixture) -> SocketAddr {
        let host = Arc::new(Host::new(HostConfig {
            shell: Shell::Serve,
            read_only: false,
            env: fx.env(),
            watch_timeout: std::time::Duration::from_secs(1),
        }));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(host, TOKEN.to_owned(), addr);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    async fn send(
        addr: SocketAddr,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> Reply {
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
            head.push_str(&format!("Host: {addr}\r\n"));
        }
        for (key, value) in extra {
            head.push_str(&format!("{key}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(head.as_bytes()).await.unwrap();
        // The server may answer 413 before it reads the whole body, so a late write can fail.
        let _ = stream.write_all(body).await;
        let mut raw = Vec::new();
        let _ = stream.read_to_end(&mut raw).await;
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (headers, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = headers
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        Reply {
            status,
            headers: headers.to_lowercase(),
            body: body.to_owned(),
        }
    }

    fn auth() -> (&'static str, String) {
        ("Authorization", format!("Bearer {TOKEN}"))
    }

    const SESSION: &str = r#"{"op":"session","args":null}"#;

    #[tokio::test]
    async fn rpc_needs_the_token() {
        let fx = Fixture::new();
        let addr = start(&fx).await;

        let (key, value) = auth();
        let ok = send(
            addr,
            "POST",
            "/api/rpc",
            &[(key, &value)],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(ok.status, 200);
        assert!(ok.body.contains(r#""shell":"serve""#), "{}", ok.body);

        let none = send(addr, "POST", "/api/rpc", &[], SESSION.as_bytes()).await;
        assert_eq!(none.status, 401);
        let wrong = send(
            addr,
            "POST",
            "/api/rpc",
            &[("Authorization", "Bearer nope")],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(wrong.status, 401);
        let empty = send(
            addr,
            "POST",
            "/api/rpc",
            &[("Authorization", "Bearer ")],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(empty.status, 401);
    }

    #[tokio::test]
    async fn scan_over_http_returns_the_snapshot() {
        let fx = Fixture::new();
        let addr = start(&fx).await;
        let (key, value) = auth();
        let body = format!(r#"{{"op":"scan","args":{{"cwd":"{}"}}}}"#, fx.project());
        let reply = send(addr, "POST", "/api/rpc", &[(key, &value)], body.as_bytes()).await;
        assert_eq!(reply.status, 200);
        let snapshot: Snapshot = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(snapshot.agents.len(), 3);
    }

    #[tokio::test]
    async fn a_foreign_origin_or_host_is_forbidden() {
        let fx = Fixture::new();
        let addr = start(&fx).await;
        let (key, value) = auth();
        let origin = send(
            addr,
            "POST",
            "/api/rpc",
            &[(key, &value), ("Origin", "http://evil.example")],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(origin.status, 403);
        let same = format!("http://{addr}");
        let ok = send(
            addr,
            "POST",
            "/api/rpc",
            &[(key, &value), ("Origin", &same)],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(ok.status, 200);
        let rebound = send(
            addr,
            "POST",
            "/api/rpc",
            &[(key, &value), ("Host", "evil.example")],
            SESSION.as_bytes(),
        )
        .await;
        assert_eq!(rebound.status, 403);
        let rebound_page = send(addr, "GET", "/", &[("Host", "evil.example")], b"").await;
        assert_eq!(rebound_page.status, 403);
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused() {
        let fx = Fixture::new();
        let addr = start(&fx).await;
        let (key, value) = auth();
        let big = vec![b' '; MAX_BODY_BYTES + 1];
        let reply = send(addr, "POST", "/api/rpc", &[(key, &value)], &big).await;
        assert_eq!(reply.status, 413);
    }

    #[tokio::test]
    async fn assets_need_no_token_and_carry_the_security_headers() {
        let fx = Fixture::new();
        let addr = start(&fx).await;
        let reply = send(addr, "GET", "/", &[], b"").await;
        assert_eq!(reply.status, 200);
        assert!(
            reply
                .headers
                .contains("content-security-policy: default-src 'self'"),
            "{}",
            reply.headers
        );
        assert!(reply.headers.contains("referrer-policy: no-referrer"));
        assert!(reply.headers.contains("x-content-type-options: nosniff"));
        assert_eq!(send(addr, "GET", "/missing.js", &[], b"").await.status, 404);
        // Unauthenticated and unknown API paths never fall through to the page.
        assert_eq!(send(addr, "GET", "/api/other", &[], b"").await.status, 401);
    }

    #[test]
    fn tokens_are_random_hex_unless_pinned() {
        let a = token(None);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, token(None));
        assert_eq!(token(Some("pin".into())), "pin");
        assert_eq!(token(Some(String::new())).len(), 64);
    }
}
