use crate::cache::{Cache, CachedEntry};
use axum::http::{HeaderValue, Method, StatusCode, header};
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
    let accepts_br = client_accepts_brotli(&req);

    if let Some(entry) = cache.get(&key) {
        if accepts_br {
            let mut resp = Response::new(Body::from(entry.body.clone()));
            *resp.headers_mut() = entry.headers.clone();
            resp.headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("br"));
            return resp;
        }
        if let Ok(decompressed) = brotli_decompress(&entry.body) {
            let mut resp = Response::new(Body::from(decompressed));
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
                if let Ok(compressed) = brotli_compress(&bytes) {
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

fn client_accepts_brotli(req: &Request) -> bool {
    req.headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(',').any(|part| {
                part.split(';')
                    .next()
                    .map(|enc| enc.trim() == "br")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn brotli_compress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 5,
        ..Default::default()
    };
    let mut output = Vec::new();
    brotli::BrotliCompress(&mut &data[..], &mut output, &params)?;
    Ok(output)
}

fn brotli_decompress(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    brotli::BrotliDecompress(&mut &data[..], &mut output)?;
    Ok(output)
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

    #[tokio::test]
    async fn br_client_gets_compressed_response() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());

        // Populate cache with a plain GET (no Accept-Encoding)
        app.clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(cache.get("/").is_some());

        // br-capable client hits the cache — should receive compressed bytes
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::ACCEPT_ENCODING, "gzip, deflate, br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("br"),
        );
    }

    #[tokio::test]
    async fn non_br_client_gets_decompressed_response() {
        let cache = Arc::new(Cache::new());
        let app = make_app(cache.clone());

        // Populate cache
        app.clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        // Non-br client — should receive raw bytes with no Content-Encoding
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(header::CONTENT_ENCODING).is_none());
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"ok");
    }
}
