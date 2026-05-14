use crate::cache::{Cache, CachedEntry};
use axum::http::{Method, StatusCode, header};
use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use std::sync::Arc;

const MAX_CACHEABLE_BODY_BYTES: usize = 4 * 1024 * 1024; // 4 MiB

pub async fn middleware(req: Request, next: Next, cache: Arc<Cache>) -> Response {
    // Only cache GET requests — HEAD responses have no body (wrong to replay),
    // and no other methods are expected to be idempotent page renders.
    if req.method() != Method::GET {
        return next.run(req).await;
    }

    if req.uri().path() == "/search" {
        return next.run(req).await;
    }

    let key = req.uri().to_string();

    if let Some(entry) = cache.get(&key) {
        if let Ok(body) = zstd::decode_all(entry.body.as_slice()) {
            let mut resp = Response::new(Body::from(body));
            *resp.headers_mut() = entry.headers.clone();
            return resp;
        }
    }

    let response = next.run(req).await;

    if response.status() != StatusCode::OK {
        return response;
    }

    let (parts, body) = response.into_parts();
    match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => {
            if bytes.len() <= MAX_CACHEABLE_BODY_BYTES {
                if let Ok(compressed) = zstd::encode_all(bytes.as_ref(), 3) {
                    let mut store_headers = parts.headers.clone();
                    store_headers.remove(header::CONTENT_LENGTH);
                    store_headers.remove(header::TRANSFER_ENCODING);
                    store_headers.remove(header::CONTENT_ENCODING);
                    cache.set(
                        key,
                        Arc::new(CachedEntry {
                            headers: store_headers,
                            body: compressed,
                        }),
                    );
                }
            }
            Response::from_parts(parts, Body::from(bytes))
        }
        Err(e) => {
            tracing::error!("cache: failed to read response body: {e}");
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;
    use axum::http::Request;
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    fn make_app(cache: Arc<Cache>) -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .route("/search", get(|| async { "search results" }))
            .route("/fail", get(|| async { StatusCode::NOT_FOUND }))
            .layer(axum::middleware::from_fn(move |req, next| {
                let c = cache.clone();
                async move { middleware(req, next, c).await }
            }))
    }

    #[tokio::test]
    async fn ok_response_is_cached() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());
        let resp = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            cache.get("/").is_some(),
            "cache should have an entry after 200 response"
        );
    }

    #[tokio::test]
    async fn search_is_not_cached() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());
        let resp = app
            .oneshot(Request::get("/search").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            cache.get("/search").is_none(),
            "search must never be cached"
        );
    }

    #[tokio::test]
    async fn non_200_is_not_cached() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());
        let resp = app
            .oneshot(Request::get("/fail").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert!(cache.get("/fail").is_none(), "non-200 must not be cached");
    }

    #[tokio::test]
    async fn cache_hit_returns_same_body() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());

        let resp1 = app
            .clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body1 = axum::body::to_bytes(resp1.into_body(), usize::MAX)
            .await
            .unwrap();

        let resp2 = app
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body2 = axum::body::to_bytes(resp2.into_body(), usize::MAX)
            .await
            .unwrap();

        assert_eq!(body1, body2, "cache hit must return same body as original");
    }

    #[tokio::test]
    async fn query_string_is_part_of_cache_key() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());
        app.clone()
            .oneshot(Request::get("/?q=foo").body(Body::empty()).unwrap())
            .await
            .unwrap();
        app.oneshot(Request::get("/?q=bar").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(cache.get("/?q=foo").is_some());
        assert!(cache.get("/?q=bar").is_some());
    }

    #[tokio::test]
    async fn head_request_not_cached() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(cache.get("/").is_none(), "HEAD requests must not be cached");
    }
}
