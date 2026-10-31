use crate::{audit, model::CrawlRequest, store::Store};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;
#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub token: String,
}
pub fn router(app: App) -> Router {
    Router::new()
        .route("/v1/runs", post(submit))
        .route("/v1/runs/{id}", get(status))
        .route("/v1/runs/{id}/report", get(report))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/rules", get(|| async { Json(audit::rules()) }))
        .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
        .route("/health/live", get(|| async { StatusCode::NO_CONTENT }))
        .route("/health/ready", get(ready))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(app)
}
async fn authorize(State(app): State<App>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !bool::from(
        Sha256::digest(presented.as_bytes()).ct_eq(&Sha256::digest(app.token.as_bytes())),
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        )
            .into_response();
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
async fn submit(State(app): State<App>, Json(r): Json<CrawlRequest>) -> Response {
    if r.validate().is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_crawl_request","detail":"Check URL and crawl limits"})),
        )
            .into_response();
    }
    match app.store.submit(&r).await {
        Ok(_) => (
            StatusCode::ACCEPTED,
            Json(json!({"request_id":r.request_id})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"request_conflict_or_queue_full"})),
        )
            .into_response(),
    }
}
async fn status(State(app): State<App>, Path(id): Path<Uuid>) -> Response {
    match app.store.view(&id.to_string()).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn report(State(app): State<App>, Path(id): Path<Uuid>) -> Response {
    match app.store.report(&id.to_string()).await {
        Ok(Some(body)) => ([("content-type", "application/json")], body).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn cancel(State(app): State<App>, Path(id): Path<Uuid>) -> Response {
    match app.store.cancel(&id.to_string()).await {
        Ok(true) => StatusCode::ACCEPTED.into_response(),
        Ok(false) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn ready(State(app): State<App>) -> StatusCode {
    if app.store.ping().await.is_ok() {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
