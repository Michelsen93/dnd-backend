use axum::Router;
use axum::http::{HeaderValue, Method, header};
use axum_extra::extract::cookie::Key;
use backend::{config::AppConfig, db, routes, state::AppState};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
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
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind(config.bind_address()).await?;
    tracing::info!(address = %listener.local_addr()?, "backend listening");
    axum::serve(listener, app).await?;
    Ok(())
}

fn cookie_key_from_secret(secret: &str) -> Key {
    let mut bytes = [0_u8; 64];
    for (index, value) in secret.as_bytes().iter().enumerate().take(64) {
        bytes[index] = *value;
    }
    Key::from(&bytes)
}
