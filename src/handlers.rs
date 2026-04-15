//! HTTP handlers. Testable: the `AppState` injects a clock, so no handler
//! touches the wall clock directly.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rand::TryRng;
use rand::rngs::SysRng;

use crate::config::Config;
use crate::db::{ConsumeError, Db};
use crate::pow;

pub const NONCE_FIELD: &str = "_sloos_nonce";
pub const POW_FIELD: &str = "_sloos_pow";
pub const NONCE_BYTES: usize = 16;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type NonceGen = Arc<dyn Fn() -> [u8; NONCE_BYTES] + Send + Sync>;
pub type Callback = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Db>>,
    pub config: Arc<Config>,
    pub clock: Clock,
    pub nonce_gen: NonceGen,
    pub callback: Callback,
}

impl AppState {
    pub fn system(db: Db, config: Config) -> Self {
        let cb_cmd = config.submit_callback.clone();
        let callback: Callback = Arc::new(move |_nonce| {
            if let Some(cmd) = cb_cmd.clone() {
                tokio::spawn(async move {
                    run_callback(&cmd).await;
                });
            }
        });
        AppState {
            db: Arc::new(Mutex::new(db)),
            config: Arc::new(config),
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
            nonce_gen: Arc::new(|| {
                let mut buf = [0u8; NONCE_BYTES];
                SysRng.try_fill_bytes(&mut buf).expect("system rng failed");
                buf
            }),
            callback,
        }
    }
}

async fn run_callback(cmd: &str) {
    let status = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .await;
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!("submit callback exited with non-zero status: {s}"),
        Err(e) => tracing::warn!("failed to spawn submit callback: {e}"),
    }
}

pub async fn get_nonce(State(state): State<AppState>) -> Response {
    let now = (state.clock)();
    let nonce_bytes = (state.nonce_gen)();
    let nonce = hex::encode(nonce_bytes);
    let expires_at = now + state.config.nonce_expiration_seconds;
    let difficulty = state.config.pow_difficulty;

    let insert_res = {
        let db = state.db.lock().expect("db mutex poisoned");
        db.insert_nonce(&nonce, difficulty, now, expires_at)
    };
    if let Err(e) = insert_res {
        tracing::error!("failed to insert nonce: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
    }

    let body = format!(
        "{{\"nonce\":\"{nonce}\",\"difficulty\":{difficulty},\"expires_at\":{expires_at}}}"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, body).into_response()
}

/// POST handler. Takes the raw bytes of the request body and parses them as
/// application/x-www-form-urlencoded.
pub async fn post_submission(State(state): State<AppState>, body: Bytes) -> Response {
    let now = (state.clock)();
    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid utf-8").into_response(),
    };
    let fields = parse_form(body_str);

    let nonce = match find_field(&fields, NONCE_FIELD) {
        Some(n) => n,
        None => return (StatusCode::BAD_REQUEST, "missing nonce").into_response(),
    };
    let pow_val = match find_field(&fields, POW_FIELD) {
        Some(p) => p,
        None => return (StatusCode::BAD_REQUEST, "missing pow").into_response(),
    };

    // Basic sanity on nonce format.
    if nonce.len() != NONCE_BYTES * 2 || hex::decode(&nonce).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid nonce format").into_response();
    }

    let difficulty = {
        let mut db = state.db.lock().expect("db mutex poisoned");
        match db.consume_nonce(&nonce, now) {
            Ok(d) => d,
            Err(ConsumeError::NotFound) => {
                return (StatusCode::BAD_REQUEST, "nonce not found").into_response();
            }
            Err(ConsumeError::Expired) => {
                return (StatusCode::BAD_REQUEST, "nonce expired").into_response();
            }
            Err(ConsumeError::AlreadyUsed) => {
                return (StatusCode::BAD_REQUEST, "nonce already used").into_response();
            }
        }
    };

    match pow::verify(&nonce, &pow_val, difficulty) {
        Ok(true) => {}
        Ok(false) => return (StatusCode::BAD_REQUEST, "invalid pow").into_response(),
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid pow format").into_response(),
    }

    // Strip the sloos-specific fields and store the rest verbatim.
    let stored = serialize_data_fields(&fields);

    {
        let db = state.db.lock().expect("db mutex poisoned");
        if let Err(e) = db.insert_submission(&nonce, &stored, now) {
            tracing::error!("failed to insert submission: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    }

    (state.callback)(nonce);

    (StatusCode::OK, "ok").into_response()
}

fn find_field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
}

fn serialize_data_fields(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (k, v) in fields {
        if k == NONCE_FIELD || k == POW_FIELD {
            continue;
        }
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

/// Minimal x-www-form-urlencoded parser. Returns fields in original order.
pub fn parse_form(input: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if input.is_empty() {
        return out;
    }
    for pair in input.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((a, b)) => (a, b),
            None => (pair, ""),
        };
        out.push((percent_decode(k), percent_decode(v)));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = hex_val(bytes[i + 1]);
                let lo = hex_val(bytes[i + 2]);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4) | l);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn parse_form_basic() {
        let fields = parse_form("a=1&b=two&c=");
        assert_eq!(
            fields,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "two".to_string()),
                ("c".to_string(), "".to_string())
            ]
        );
    }

    #[test]
    fn parse_form_percent_decodes() {
        let fields = parse_form("name=hello%20world&key=a%2Bb");
        assert_eq!(fields[0].1, "hello world");
        assert_eq!(fields[1].1, "a+b");
    }

    #[test]
    fn parse_form_plus_is_space() {
        let fields = parse_form("x=a+b+c");
        assert_eq!(fields[0].1, "a b c");
    }

    #[test]
    fn parse_form_empty_string() {
        assert!(parse_form("").is_empty());
    }

    #[test]
    fn parse_form_no_value() {
        let fields = parse_form("flag");
        assert_eq!(fields, vec![("flag".to_string(), "".to_string())]);
    }

    #[test]
    fn serialize_strips_sloos_fields() {
        let fields = vec![
            ("_sloos_nonce".to_string(), "abc".to_string()),
            ("_sloos_pow".to_string(), "def".to_string()),
            ("name".to_string(), "alice".to_string()),
            ("msg".to_string(), "hi there".to_string()),
        ];
        let s = serialize_data_fields(&fields);
        assert_eq!(s, "name=alice&msg=hi+there");
    }

    #[test]
    fn percent_encode_special_chars() {
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(percent_encode("hi there"), "hi+there");
    }

    use crate::config::Config;
    use crate::db::Db;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state(
        difficulty: u32,
        now: i64,
        expiration: i64,
    ) -> (AppState, Arc<Mutex<Vec<String>>>) {
        let db = Db::open_in_memory().unwrap();
        let cfg = Config {
            db_path: ":memory:".into(),
            pow_difficulty: difficulty,
            nonce_expiration_seconds: expiration,
            submit_callback: None,
            bind_addr: "0.0.0.0:0".into(),
        };
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_for_cb = calls.clone();
        let callback: Callback = Arc::new(move |n| {
            calls_for_cb.lock().unwrap().push(n);
        });
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            config: Arc::new(cfg),
            clock: Arc::new(move || now),
            nonce_gen: Arc::new(|| [0xAA; NONCE_BYTES]),
            callback,
        };
        (state, calls)
    }

    #[tokio::test]
    async fn get_nonce_returns_expected_json() {
        let (state, _) = test_state(3, 1_000, 60);
        let app = crate::router(state);
        let res = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), 1024).await.unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        assert_eq!(
            s,
            r#"{"nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","difficulty":3,"expires_at":1060}"#
        );
    }

    #[tokio::test]
    async fn post_submission_happy_path() {
        let (state, calls) = test_state(4, 1_000, 60);
        let app = crate::router(state);
        // Solve a PoW for the well-known nonce.
        let nonce_bytes = [0xAAu8; NONCE_BYTES];
        let nonce_hex = hex::encode(nonce_bytes);
        let pow = crate::pow::solve(&nonce_bytes, 4);
        let pow_hex = hex::encode(&pow);

        // First GET to register the nonce.
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body = format!("_sloos_nonce={nonce_hex}&_sloos_pow={pow_hex}&name=alice&msg=hi+there");
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn post_submission_rejects_bad_pow() {
        let (state, _) = test_state(8, 1_000, 60);
        let app = crate::router(state);
        // Register nonce.
        app.clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let nonce_hex = hex::encode([0xAAu8; NONCE_BYTES]);
        let body = format!("_sloos_nonce={nonce_hex}&_sloos_pow=00");
        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_submission_rejects_replay() {
        let (state, _) = test_state(2, 1_000, 60);
        let app = crate::router(state);
        app.clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let nonce_bytes = [0xAAu8; NONCE_BYTES];
        let nonce_hex = hex::encode(nonce_bytes);
        let pow = crate::pow::solve(&nonce_bytes, 2);
        let pow_hex = hex::encode(&pow);

        let body = format!("_sloos_nonce={nonce_hex}&_sloos_pow={pow_hex}");
        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        let replay = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    }

    proptest! {
        // Round-trip: percent_encode then parse_form yields original pairs.
        #[test]
        fn form_roundtrip(
            pairs in proptest::collection::vec(
                (
                    "[a-zA-Z][a-zA-Z0-9_]{0,8}",
                    "[^\0]{0,16}",
                ),
                0..6,
            )
        ) {
            let encoded: String = pairs
                .iter()
                .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
                .collect::<Vec<_>>()
                .join("&");
            let decoded = parse_form(&encoded);
            let expected: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            prop_assert_eq!(decoded, expected);
        }
    }
}
