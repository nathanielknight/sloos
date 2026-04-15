pub mod config;
pub mod db;
pub mod handlers;
pub mod pow;

use axum::Router;
use axum::routing::{get, post};

pub fn router(state: handlers::AppState) -> Router {
    Router::new()
        .route("/", get(handlers::get_nonce))
        .route("/", post(handlers::post_submission))
        .with_state(state)
}
