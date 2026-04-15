use std::time::Duration;

use sloos::config::{self, SystemEnv};
use sloos::db::Db;
use sloos::handlers::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cfg = config::load(&SystemEnv)?;
    tracing::info!(
        "starting sloos: db={} difficulty={} expiry={}s callback={}",
        cfg.db_path,
        cfg.pow_difficulty,
        cfg.nonce_expiration_seconds,
        cfg.submit_callback.as_deref().unwrap_or("<none>")
    );
    let db = Db::open(&cfg.db_path)?;
    let bind_addr = cfg.bind_addr.clone();
    let state = AppState::system(db, cfg);

    // Pruning task: every 15 minutes.
    let prune_state = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15 * 60));
        // skip the first tick so we don't immediately prune an empty db
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = (prune_state.clock)();
            let result = {
                let Ok(db) = prune_state.db.lock() else {
                    tracing::error!("db mutex poisoned, skipping prune");
                    continue;
                };
                db.prune_expired(now)
            };
            match result {
                Ok(n) if n > 0 => tracing::info!("pruned {n} expired nonces"),
                Ok(_) => {}
                Err(e) => tracing::warn!("prune failed: {e}"),
            }
        }
    });

    let router = sloos::router(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!("listening on {}", listener.local_addr()?);
    axum::serve(listener, router).await?;

    Ok(())
}
