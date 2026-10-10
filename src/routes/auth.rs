use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use axum_extra::extract::PrivateCookieJar;

use crate::{
    auth::{
        add_session_cookie, clear_session_cookie, hash_password, require_user, verify_password,
    },
    db::UserRow,
    error::ApiError,
    models::{AuthResponse, Credentials, MessageResponse, UserResponse},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/me", get(me))
}

pub async fn register(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<Credentials>,
) -> Result<impl IntoResponse, ApiError> {
    let email = input.email.trim().to_lowercase();
    if email.is_empty() || input.password.len() < 8 {
        return Err(ApiError::bad_request(
            "Email is required and password must be at least 8 characters",
        ));
    }

    let existing = sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE email = ?")
        .bind(&email)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(ApiError::bad_request(
            "An account with this email already exists",
        ));
    }

    let now = crate::models::now_iso();
    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash = hash_password(&input.password)?;

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&now)
    .bind(&now)
    .execute(&state.pool)
    .await?;

    let user = UserResponse {
        id: user_id.clone(),
        email,
        created_at: now.clone(),
        updated_at: now,
    };

    Ok((
        StatusCode::CREATED,
        (
            add_session_cookie(jar, &user_id, state.config.cookie_secure),
            Json(AuthResponse { user }),
        ),
    ))
}

pub async fn login(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Json(input): Json<Credentials>,
) -> Result<impl IntoResponse, ApiError> {
    let email = input.email.trim().to_lowercase();
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, password_hash, created_at, updated_at FROM users WHERE email = ?",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("Invalid email or password"))?;

    if !verify_password(&input.password, &user.password_hash)? {
        return Err(ApiError::unauthorized("Invalid email or password"));
    }

    Ok((
        add_session_cookie(jar, &user.id, state.config.cookie_secure),
        Json(AuthResponse {
            user: UserResponse {
                id: user.id,
                email: user.email,
                created_at: user.created_at,
                updated_at: user.updated_at,
            },
        }),
    ))
}

pub async fn logout(jar: PrivateCookieJar) -> impl IntoResponse {
    (
        clear_session_cookie(jar),
        Json(MessageResponse {
            message: "Logged out".to_string(),
        }),
    )
}

pub async fn me(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
) -> Result<Json<AuthResponse>, ApiError> {
    let user = require_user(&state.pool, &jar).await?;
    Ok(Json(AuthResponse {
        user: UserResponse {
            id: user.id,
            email: user.email,
            created_at: user.created_at,
            updated_at: user.updated_at,
        },
    }))
}
