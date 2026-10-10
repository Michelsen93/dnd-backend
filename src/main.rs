use axum::Router;
use axum::http::{HeaderValue, Method, header};
use axum_extra::extract::cookie::Key;
use backend::{config::AppConfig, db, routes, state::AppState};
use tower_http::{cors::CorsLayer, set_header::SetResponseHeaderLayer, trace::TraceLayer};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "backend=debug,tower_http=debug".to_string()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = AppConfig::from_env();
    if config.cookie_secure && std::env::var("COOKIE_SECRET").is_err() {
        return Err("COOKIE_SECRET must be set in production (COOKIE_SECURE=true)".into());
    }
    if config.cookie_secure && config.firebase_project_id.is_none() {
        // Without Firebase the passwordless dev login would be open to the internet.
        return Err("FIREBASE_PROJECT_ID must be set in production (COOKIE_SECURE=true)".into());
    }
    let pool = db::connect(&config.database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let shared_state = AppState::new(
        pool,
        cookie_key_from_secret(&config.cookie_secret),
        config.clone(),
    );

    let cors = CorsLayer::new()
        .allow_origin(HeaderValue::from_str(&config.allowed_origin)?)
        .allow_credentials(true)
        .allow_headers([header::ACCEPT, header::CONTENT_TYPE])
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ]);

    let app: Router = routes::router()
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        // API responses are per-user; never let a CDN (Firebase Hosting) cache them.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind(config.bind_address()).await?;
    tracing::info!(address = %listener.local_addr()?, "backend listening");
    // Exit promptly on SIGTERM/Ctrl-C instead of draining: live SSE streams never finish on their
    // own, and in the container Litestream needs the server gone to run its final sync before
    // Cloud Run's 10 s shutdown deadline.
    tokio::select! {
        result = axum::serve(listener, app) => result?,
        _ = shutdown_signal() => tracing::info!("shutdown signal received, exiting"),
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sigterm) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sigterm.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

fn cookie_key_from_secret(secret: &str) -> Key {
    let mut bytes = [0_u8; 64];
    for (index, value) in secret.as_bytes().iter().enumerate().take(64) {
        bytes[index] = *value;
    }
    Key::from(&bytes)
}
