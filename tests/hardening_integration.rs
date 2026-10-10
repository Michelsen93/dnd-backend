mod common;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use common::test_app;
use serde_json::json;
use tower::ServiceExt;

#[tokio::test]
async fn anonymous_and_sign_in_traffic_is_rate_limited() {
    let app = test_app().await;

    // Sign-in attempts: 10 per minute per address.
    for i in 0..10 {
        let (status, _) = app
            .call(
                "POST",
                "/api/auth/dev-login",
                None,
                Some(json!({ "email": format!("u{i}@example.com") })),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, body) = app
        .call(
            "POST",
            "/api/auth/dev-login",
            None,
            Some(json!({ "email": "late@example.com" })),
        )
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");

    // Other anonymous requests: 60 per minute per address (11 already used above).
    let mut limited = None;
    for i in 0..60 {
        let response = app
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/auth/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            assert!(response.headers().contains_key(header::RETRY_AFTER));
            limited = Some(i);
            break;
        }
    }
    assert_eq!(
        limited,
        Some(49),
        "the 61st anonymous request in a minute is refused"
    );

    // A different client address has its own budget.
    let response = app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/auth/config")
                .header("x-forwarded-for", "203.0.113.9")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn oversized_requests_are_rejected() {
    let app = test_app().await;
    let cookie = app.register("big@example.com").await;
    let huge = "x".repeat(300 * 1024);
    let (status, _) = app
        .call(
            "POST",
            "/api/campaigns",
            Some(&cookie),
            Some(json!({ "name": huge })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    let (status, body) = app
        .call(
            "POST",
            "/api/campaigns",
            Some(&cookie),
            Some(json!({ "name": "x".repeat(81) })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let mut character = common::character_json("Pip");
    character["backstory"] = json!("y".repeat(70 * 1024));
    let (status, _) = app
        .call("POST", "/api/characters", Some(&cookie), Some(character))
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "character payloads are capped at 64 KB"
    );
}

#[tokio::test]
async fn per_account_quotas() {
    let app = test_app().await;
    let cookie = app.register("hoarder@example.com").await;

    for i in 0..10 {
        app.ok(
            "POST",
            "/api/campaigns",
            &cookie,
            Some(json!({ "name": format!("Campaign {i}") })),
        )
        .await;
    }
    let (status, body) = app
        .call(
            "POST",
            "/api/campaigns",
            Some(&cookie),
            Some(json!({ "name": "One too many" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("Limit reached"));

    for i in 0..30 {
        app.create_character(&cookie, &format!("Hero {i}")).await;
    }
    let (status, _) = app
        .call(
            "POST",
            "/api/characters",
            Some(&cookie),
            Some(common::character_json("Hero 31")),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_feed_is_pruned_to_a_bounded_size() {
    let app = test_app().await;
    let dm = app.register("dm@example.com").await;
    let campaign = app
        .ok(
            "POST",
            "/api/campaigns",
            &dm,
            Some(json!({ "name": "Chatty" })),
        )
        .await;
    let id = campaign["id"].as_str().unwrap();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5000)
         INSERT INTO campaign_events (id, campaign_id, kind, visibility, payload, created_at)
         SELECT 'old-' || i, ?, 'note', 'public', '{}', '' FROM n",
    )
    .bind(id)
    .execute(&app.pool)
    .await
    .unwrap();
    app.ok(
        "POST",
        &format!("/api/campaigns/{id}/events"),
        &dm,
        Some(json!({ "text": "hello" })),
    )
    .await;
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_events WHERE campaign_id = ?")
            .bind(id)
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(count, 5000);
    let oldest_gone: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_events WHERE id = 'old-1'")
            .fetch_one(&app.pool)
            .await
            .unwrap();
    assert_eq!(oldest_gone, 0);

    let (status, _) = app
        .call(
            "POST",
            &format!("/api/campaigns/{id}/events"),
            Some(&dm),
            Some(json!({ "text": "z".repeat(501) })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
