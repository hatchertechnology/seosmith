//! Service-authenticated Seosmith ingestion and owner-scoped report reads.
use crate::websites::WebsiteStore;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    delivery_id: Uuid,
    sha256: String,
    report_json: String,
}

pub(crate) fn ingest_router(store: WebsiteStore) -> Router {
    Router::new()
        .route("/integrations/seosmith/audits", post(ingest))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(store)
}
pub(crate) fn read_router(store: WebsiteStore) -> Router {
    Router::new()
        .route("/websites/{id}/seo/audits", get(read))
        .with_state(store)
}
async fn ingest(
    State(store): State<WebsiteStore>,
    headers: HeaderMap,
    Json(body): Json<Envelope>,
) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| s.len() >= 32 && s.len() <= 512)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if headers.get("idempotency-key").and_then(|h| h.to_str().ok())
        != Some(body.delivery_id.to_string().as_str())
        || body.report_json.len() > 16 * 1024 * 1024
        || body.sha256.len() != 64
        || !body
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid_delivery"})),
        )
            .into_response();
    }
    let response=store.ingest_seosmith(json!({"p_token":token,"p_delivery_id":body.delivery_id,"p_sha256":body.sha256,"p_report_json":body.report_json})).await;
    match response {
        Ok(value) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Err(e) => e.into_response(),
    }
}
#[derive(Deserialize)]
struct PageQuery {
    limit: Option<u32>,
    before: Option<String>,
    before_id: Option<Uuid>,
}
async fn read(
    State(store): State<WebsiteStore>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<PageQuery>,
) -> Response {
    match store.rpc(&headers,"read_seosmith_audits",json!({"p_website_id":id,"p_limit":q.limit.unwrap_or(10),"p_before":q.before,"p_before_id":q.before_id})).await {
        Ok(value)=>([(header::CACHE_CONTROL,"no-store")],Json(value)).into_response(),Err(e)=>e.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    #[tokio::test]
    async fn delivery_requires_worker_credential_and_idempotency_key() {
        let app = ingest_router(WebsiteStore::new("http://127.0.0.1:1", "test"));
        let id = "00000000-0000-0000-0000-000000000001";
        let payload = json!({"delivery_id":id,"sha256":"a".repeat(64),"report_json":"{}"});
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/integrations/seosmith/audits")
                .header("content-type", "application/json")
        };
        let response = app
            .clone()
            .oneshot(request().body(Body::from(payload.to_string())).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let response = app
            .oneshot(
                request()
                    .header("authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    #[tokio::test]
    async fn valid_envelope_is_forwarded_to_only_the_credential_checked_rpc() {
        let id = "00000000-0000-0000-0000-000000000001";
        let rpc=Router::new().route("/rest/v1/rpc/ingest_seosmith_audit",post(|headers:HeaderMap,Json(body):Json<serde_json::Value>|async move {
            assert_eq!(headers["apikey"],"publishable");assert!(!headers.contains_key("authorization"));
            assert_eq!(body["p_token"],"x".repeat(32));
            Json(json!({"delivery_id":body["p_delivery_id"],"sha256":body["p_sha256"],"accepted":true}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, rpc).await.unwrap() });
        let app = ingest_router(WebsiteStore::new(&url, "publishable"));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/integrations/seosmith/audits")
                    .header("content-type", "application/json")
                    .header("idempotency-key", id)
                    .header("authorization", format!("Bearer {}", "x".repeat(32)))
                    .body(Body::from(
                        json!({"delivery_id":id,"sha256":"a".repeat(64),"report_json":"{}"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["accepted"], true);
        server.abort();
    }
}
