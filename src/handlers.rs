//! HTTP handlers. Testable: the `AppState` injects a clock, so no handler
//! touches the wall clock directly.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::config::Config;
use crate::db::{ConsumeError, Db};
use crate::pow;

pub const NONCE_FIELD: &str = "_sloos_nonce";
pub const POW_FIELD: &str = "_sloos_pow";
pub const NONCE_BYTES: usize = 16;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type Callback = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Mutex<Db>>,
    pub config: Arc<Config>,
    pub clock: Clock,
    pub callback: Callback,
}

impl AppState {
    pub fn system(db: Db, config: Config) -> Self {
        let cb_cmd = config.submit_callback.clone();
        let callback: Callback = Arc::new(move || {
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
            callback,
        }
    }
}

fn generate_nonce() -> Result<[u8; NONCE_BYTES], getrandom::Error> {
    let mut buf = [0u8; NONCE_BYTES];
    getrandom::fill(&mut buf)?;
    Ok(buf)
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
    let nonce_bytes = match generate_nonce() {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("nonce generation failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        }
    };
    let nonce = hex::encode(nonce_bytes);
    let expires_at = now + state.config.nonce_expiration_seconds;
    let difficulty = state.config.pow_difficulty;

    let insert_res = {
        let db = match state.db.lock() {
            Ok(db) => db,
            Err(e) => {
                tracing::error!("db mutex poisoned: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            }
        };
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
    let nonce = match extract_raw_field(body_str, NONCE_FIELD) {
        Some(n) => n.to_string(),
        None => return (StatusCode::BAD_REQUEST, "missing nonce").into_response(),
    };
    let pow_val = match extract_raw_field(body_str, POW_FIELD) {
        Some(p) => p.to_string(),
        None => return (StatusCode::BAD_REQUEST, "missing pow").into_response(),
    };

    // Basic sanity on nonce format.
    if nonce.len() != NONCE_BYTES * 2 || hex::decode(&nonce).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid nonce format").into_response();
    }

    let difficulty = {
        let mut db = match state.db.lock() {
            Ok(db) => db,
            Err(e) => {
                tracing::error!("db mutex poisoned: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            }
        };
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

    // Strip the sloos-specific fields and store the raw form body.
    let stored = strip_sloos_fields(body_str);

    {
        let db = match state.db.lock() {
            Ok(db) => db,
            Err(e) => {
                tracing::error!("db mutex poisoned: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            }
        };
        if let Err(e) = db.insert_submission(&nonce, &stored, now) {
            tracing::error!("failed to insert submission: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    }

    (state.callback)();

    (StatusCode::OK, "ok").into_response()
}

/// Extract the raw value of a field from a form body without decoding.
/// Safe for fields whose values are known to be plain ASCII (like hex strings).
fn extract_raw_field<'a>(raw: &'a str, name: &str) -> Option<&'a str> {
    raw.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == name { Some(v) } else { None }
    })
}

/// Strip `_sloos_*` fields from the raw form body, preserving encoding.
fn strip_sloos_fields(raw: &str) -> String {
    raw.split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            key != NONCE_FIELD && key != POW_FIELD
        })
        .collect::<Vec<_>>()
        .join("&")
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extract_raw_field_finds_value() {
        let raw = "_sloos_nonce=abc123&_sloos_pow=def456&name=alice";
        assert_eq!(extract_raw_field(raw, "_sloos_nonce"), Some("abc123"));
        assert_eq!(extract_raw_field(raw, "_sloos_pow"), Some("def456"));
        assert_eq!(extract_raw_field(raw, "name"), Some("alice"));
    }

    #[test]
    fn extract_raw_field_returns_none_for_missing() {
        assert_eq!(extract_raw_field("a=1&b=2", "c"), None);
        assert_eq!(extract_raw_field("", "a"), None);
    }

    #[test]
    fn strip_sloos_fields_removes_internal() {
        let raw = "_sloos_nonce=abc&_sloos_pow=def&name=alice&msg=hi+there";
        assert_eq!(strip_sloos_fields(raw), "name=alice&msg=hi+there");
    }

    #[test]
    fn strip_sloos_fields_preserves_encoding() {
        let raw = "_sloos_nonce=x&greeting=hello%20world&emoji=%F0%9F%91%8D";
        assert_eq!(strip_sloos_fields(raw), "greeting=hello%20world&emoji=%F0%9F%91%8D");
    }

    use proptest::prelude::*;

    /// Strategy for form field names: ASCII alphanumeric + underscore, non-empty,
    /// and crucially not a sloos-internal field name.
    fn user_field_name() -> impl Strategy<Value = String> {
        "[a-zA-Z][a-zA-Z0-9_]{0,8}"
            .prop_filter("must not be a sloos field", |s| {
                s != NONCE_FIELD && s != POW_FIELD
            })
    }

    /// Strategy for form field values: printable ASCII without `&` or `=`
    /// (i.e. values that don't need encoding, so raw round-tripping is exact).
    fn field_value() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9_.~+%20]{0,16}"
    }

    proptest! {
        /// extract_raw_field finds a field that was placed in a form body.
        #[test]
        fn extract_finds_present_field(
            prefix in proptest::collection::vec((user_field_name(), field_value()), 0..4),
            target_name in user_field_name(),
            target_value in field_value(),
            suffix in proptest::collection::vec((user_field_name(), field_value()), 0..4),
        ) {
            let mut pairs: Vec<String> = prefix.iter().map(|(k, v)| format!("{k}={v}")).collect();
            pairs.push(format!("{target_name}={target_value}"));
            pairs.extend(suffix.iter().map(|(k, v)| format!("{k}={v}")));
            let raw = pairs.join("&");
            // Should find the target (or an earlier field with the same name).
            let result = extract_raw_field(&raw, &target_name);
            prop_assert!(result.is_some());
        }

        /// strip_sloos_fields never leaves sloos-internal fields in the output.
        #[test]
        fn strip_never_contains_sloos_fields(
            nonce_val in "[a-f0-9]{32}",
            pow_val in "[a-f0-9]{2,16}",
            user_fields in proptest::collection::vec((user_field_name(), field_value()), 0..6),
        ) {
            let mut pairs = vec![
                format!("{NONCE_FIELD}={nonce_val}"),
                format!("{POW_FIELD}={pow_val}"),
            ];
            pairs.extend(user_fields.iter().map(|(k, v)| format!("{k}={v}")));
            let raw = pairs.join("&");
            let stripped = strip_sloos_fields(&raw);
            for pair in stripped.split('&') {
                if pair.is_empty() { continue; }
                let key = pair.split_once('=').map_or(pair, |(k, _)| k);
                prop_assert_ne!(key, NONCE_FIELD);
                prop_assert_ne!(key, POW_FIELD);
            }
        }

        /// strip_sloos_fields preserves all user fields in order.
        #[test]
        fn strip_preserves_user_fields(
            user_fields in proptest::collection::vec((user_field_name(), field_value()), 1..6),
        ) {
            let user_pairs: Vec<String> = user_fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
            let expected = user_pairs.join("&");
            let mut pairs = vec![
                format!("{NONCE_FIELD}=aabb"),
                format!("{POW_FIELD}=ccdd"),
            ];
            pairs.extend(user_pairs);
            let raw = pairs.join("&");
            let stripped = strip_sloos_fields(&raw);
            prop_assert_eq!(stripped, expected);
        }
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
        let callback: Callback = Arc::new(move || {
            calls_for_cb.lock().unwrap().push("called".to_string());
        });
        let state = AppState {
            db: Arc::new(Mutex::new(db)),
            config: Arc::new(cfg),
            clock: Arc::new(move || now),
            callback,
        };
        (state, calls)
    }

    /// Helper: GET / and parse the JSON response, returning (nonce_hex, difficulty, expires_at).
    async fn get_nonce_from(app: &axum::Router) -> (String, u32, i64) {
        let res = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), 1024).await.unwrap();
        let s = std::str::from_utf8(&body).unwrap();
        // Minimal JSON parsing — the format is fixed.
        let nonce = s.split("\"nonce\":\"").nth(1).unwrap().split('"').next().unwrap();
        let difficulty: u32 = s.split("\"difficulty\":").nth(1).unwrap().split([',', '}']).next().unwrap().parse().unwrap();
        let expires_at: i64 = s.split("\"expires_at\":").nth(1).unwrap().split('}').next().unwrap().parse().unwrap();
        (nonce.to_string(), difficulty, expires_at)
    }

    #[tokio::test]
    async fn get_nonce_returns_well_formed_json() {
        let (state, _) = test_state(3, 1_000, 60);
        let app = crate::router(state);
        let (nonce, difficulty, expires_at) = get_nonce_from(&app).await;
        assert_eq!(nonce.len(), NONCE_BYTES * 2);
        assert!(hex::decode(&nonce).is_ok());
        assert_eq!(difficulty, 3);
        assert_eq!(expires_at, 1_060);
    }

    #[tokio::test]
    async fn post_submission_happy_path() {
        let (state, calls) = test_state(4, 1_000, 60);
        let app = crate::router(state);

        let (nonce_hex, _difficulty, _) = get_nonce_from(&app).await;
        let nonce_bytes = hex::decode(&nonce_hex).unwrap();
        let pow = crate::pow::solve(&nonce_bytes, 4);
        let pow_hex = hex::encode(&pow);

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

        let (nonce_hex, _, _) = get_nonce_from(&app).await;
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

        let (nonce_hex, _, _) = get_nonce_from(&app).await;
        let nonce_bytes = hex::decode(&nonce_hex).unwrap();
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

}
