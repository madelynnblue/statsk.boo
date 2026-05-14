# HTTP Cache Middleware Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an in-memory zstd-compressed HTTP response cache as Axum middleware, invalidated after any ingest pass that writes games.

**Architecture:** A `Cache` struct (in `src/cache.rs`) holds a `RwLock<HashMap<String, Arc<CachedEntry>>>` and is shared between the web layer and the ingest loop via `Arc`. A middleware function in `src/web/cache.rs` intercepts all routes except `/search`, serving hits from cache and storing compressed bytes on misses. Three ingest functions (`reingest_stale`, `reconcile_missing`, `run_ingest`) call `cache.clear()` when they write one or more games.

**Tech Stack:** Rust, Axum 0.8, Tower 0.5, zstd, `std::sync::RwLock`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `src/cache.rs` | Create | `Cache` struct, `CachedEntry`, unit tests |
| `src/web/cache.rs` | Create | `middleware` async fn, integration tests |
| `src/web/mod.rs` | Modify | Add `cache` to `AppState`, wire middleware, update `serve` signature |
| `src/ingest/mod.rs` | Modify | `ingest_loop` takes `Arc<Cache>`, `run_ingest` returns `usize`, three `cache.clear()` sites |
| `src/main.rs` | Modify | Construct `Arc<Cache>`, pass to both `serve` and `ingest_loop` |
| `src/lib.rs` | Modify | Add `pub mod cache;` |
| `Cargo.toml` | Modify | Add `zstd`; add `tower` to dev-dependencies |

---

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add zstd**

```bash
cd /home/madelynn/code/wsb && cargo add zstd
```

- [ ] **Step 2: Add tower to dev-dependencies**

```bash
cargo add --dev tower
```

- [ ] **Step 3: Verify Cargo.toml has both**

```bash
grep -E 'zstd|tower' Cargo.toml
```

Expected: `zstd = "..."` under `[dependencies]`, `tower = "..."` under `[dev-dependencies]`.

- [ ] **Step 4: Confirm it compiles**

```bash
cargo build 2>&1 | tail -5
```

Expected: no errors.

---

### Task 2: Create `src/cache.rs` — Cache struct

**Files:**
- Create: `src/cache.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/cache.rs` with tests only:

```rust
use axum::http::HeaderMap;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;

pub struct CachedEntry {
    pub headers: HeaderMap,
    pub body: Vec<u8>, // zstd-compressed
}

pub struct Cache(RwLock<HashMap<String, Arc<CachedEntry>>>);

impl Cache {
    pub fn new() -> Self {
        Self(RwLock::new(HashMap::new()))
    }

    pub fn get(&self, key: &str) -> Option<Arc<CachedEntry>> {
        todo!()
    }

    pub fn set(&self, key: String, entry: Arc<CachedEntry>) {
        todo!()
    }

    pub fn clear(&self) {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_on_empty_returns_none() {
        let c = Cache::new();
        assert!(c.get("/foo").is_none());
    }

    #[test]
    fn set_then_get_returns_entry() {
        let c = Cache::new();
        let entry = Arc::new(CachedEntry {
            headers: HeaderMap::new(),
            body: b"compressed".to_vec(),
        });
        c.set("/foo".to_string(), entry);
        let got = c.get("/foo").unwrap();
        assert_eq!(got.body, b"compressed");
    }

    #[test]
    fn clear_removes_all_entries() {
        let c = Cache::new();
        c.set("/a".to_string(), Arc::new(CachedEntry { headers: HeaderMap::new(), body: vec![1] }));
        c.set("/b".to_string(), Arc::new(CachedEntry { headers: HeaderMap::new(), body: vec![2] }));
        c.clear();
        assert!(c.get("/a").is_none());
        assert!(c.get("/b").is_none());
    }

    #[test]
    fn set_overwrites_existing_key() {
        let c = Cache::new();
        c.set("/x".to_string(), Arc::new(CachedEntry { headers: HeaderMap::new(), body: b"first".to_vec() }));
        c.set("/x".to_string(), Arc::new(CachedEntry { headers: HeaderMap::new(), body: b"second".to_vec() }));
        assert_eq!(c.get("/x").unwrap().body, b"second");
    }
}
```

- [ ] **Step 2: Add `pub mod cache;` to `src/lib.rs`**

Edit `src/lib.rs` — add `pub mod cache;` after the existing modules:

```rust
pub mod cache;
pub mod canon;
pub mod config;
pub mod db;
pub mod ingest;
pub mod models;
pub mod web;
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test cache:: 2>&1 | tail -20
```

Expected: four tests all fail with `not yet implemented` panic.

- [ ] **Step 4: Implement `Cache` methods**

Replace the three `todo!()` bodies in `src/cache.rs`:

```rust
pub fn get(&self, key: &str) -> Option<Arc<CachedEntry>> {
    self.0.read().unwrap().get(key).cloned()
}

pub fn set(&self, key: String, entry: Arc<CachedEntry>) {
    self.0.write().unwrap().insert(key, entry);
}

pub fn clear(&self) {
    self.0.write().unwrap().clear();
}
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test cache:: 2>&1 | tail -10
```

Expected: `test result: ok. 4 passed`.

- [ ] **Step 6: cargo fmt**

```bash
cargo fmt
```

- [ ] **Step 7: Commit**

```bash
jj new -m "feat: add Cache struct with zstd-compressed entry storage"
jj file track src/cache.rs
```

---

### Task 3: Create `src/web/cache.rs` — middleware

**Files:**
- Create: `src/web/cache.rs`
- Modify: `src/web/mod.rs` (add `pub mod cache;`)

- [ ] **Step 1: Write the failing middleware tests**

Create `src/web/cache.rs`:

```rust
use crate::cache::{Cache, CachedEntry};
use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use axum::http::StatusCode;
use std::sync::Arc;

pub async fn middleware(req: Request, next: Next, cache: Arc<Cache>) -> Response {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;
    use axum::{Router, routing::get};
    use axum::http::Request;
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
        assert!(cache.get("/").is_some(), "cache should have an entry after 200 response");
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
        assert!(cache.get("/search").is_none(), "search must never be cached");
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
}
```

- [ ] **Step 2: Add `pub mod cache;` to `src/web/mod.rs`**

Add at the top of `src/web/mod.rs`:

```rust
pub mod cache;
pub mod error;
pub mod handlers;
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test web::cache:: 2>&1 | tail -20
```

Expected: five tests all fail with `not yet implemented`.

- [ ] **Step 4: Implement the middleware**

Replace the `todo!()` body in `pub async fn middleware`:

```rust
pub async fn middleware(req: Request, next: Next, cache: Arc<Cache>) -> Response {
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
            if let Ok(compressed) = zstd::encode_all(bytes.as_ref(), 3) {
                cache.set(
                    key,
                    Arc::new(CachedEntry {
                        headers: parts.headers.clone(),
                        body: compressed,
                    }),
                );
            }
            Response::from_parts(parts, Body::from(bytes))
        }
        Err(e) => {
            tracing::error!("cache: failed to read response body: {e}");
            Response::from_parts(parts, Body::empty())
        }
    }
}
```

Add the `zstd` import at the top of the file (after existing imports):

```rust
use crate::cache::{Cache, CachedEntry};
use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use axum::http::StatusCode;
use std::sync::Arc;
```

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test web::cache:: 2>&1 | tail -10
```

Expected: `test result: ok. 5 passed`.

- [ ] **Step 6: cargo fmt**

```bash
cargo fmt
```

- [ ] **Step 7: Commit**

```bash
jj new -m "feat: add cache middleware with zstd compression"
jj file track src/web/cache.rs
```

---

### Task 4: Wire cache into `AppState` and router

**Files:**
- Modify: `src/web/mod.rs`

- [ ] **Step 1: Update `src/web/mod.rs`**

Full updated file:

```rust
pub mod cache;
pub mod error;
pub mod handlers;

use crate::cache::Cache;
use crate::config::Config;
use axum::{Router, routing::get};
use minijinja::Environment;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<PgPool>,
    pub env: Arc<Environment<'static>>,
    pub cache: Arc<Cache>,
}

pub async fn serve(cfg: Arc<Config>, pool: Arc<PgPool>, cache: Arc<Cache>) -> anyhow::Result<()> {
    let env = Arc::new(build_template_env());
    let state = AppState { pool, env, cache: cache.clone() };

    let app = Router::new()
        .route("/", get(handlers::index::handle))
        .route("/search", get(handlers::search::handle))
        .route("/player", get(handlers::player::handle))
        .route("/team", get(handlers::team::handle))
        .route("/league", get(handlers::league::handle))
        .route("/leagues", get(handlers::leagues::handle))
        .route("/game/{canonical_id}", get(handlers::game::handle))
        .route("/about", get(handlers::about::handle))
        .route("/robots.txt", get(handlers::robots::handle))
        .layer(axum::middleware::from_fn(move |req, next| {
            let c = cache.clone();
            async move { cache::middleware(req, next, c).await }
        }))
        .with_state(state);

    let addr: std::net::SocketAddr = cfg.bind_addr.parse()?;
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn build_template_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("base.html", include_str!("../../templates/base.html"))
        .unwrap();
    env.add_template("index.html", include_str!("../../templates/index.html"))
        .unwrap();
    env.add_template("search.html", include_str!("../../templates/search.html"))
        .unwrap();
    env.add_template("player.html", include_str!("../../templates/player.html"))
        .unwrap();
    env.add_template("team.html", include_str!("../../templates/team.html"))
        .unwrap();
    env.add_template("league.html", include_str!("../../templates/league.html"))
        .unwrap();
    env.add_template("leagues.html", include_str!("../../templates/leagues.html"))
        .unwrap();
    env.add_template("game.html", include_str!("../../templates/game.html"))
        .unwrap();
    env.add_template("about.html", include_str!("../../templates/about.html"))
        .unwrap();
    env.add_template("robots.txt", include_str!("../../templates/robots.txt"))
        .unwrap();
    env
}
```

- [ ] **Step 2: Verify it compiles (main.rs will fail — that's expected)**

```bash
cargo build 2>&1 | grep -E 'error|warning.*unused' | head -20
```

Expected: errors about `serve` call site in `main.rs` (wrong argument count) — that's fine, fixed in the next task. No errors in `web/mod.rs` itself.

---

### Task 5: Update `src/ingest/mod.rs`

**Files:**
- Modify: `src/ingest/mod.rs`

Three changes: (1) `ingest_loop` gains `Arc<Cache>` param, (2) `run_ingest` returns `usize`, (3) three `cache.clear()` call sites.

- [ ] **Step 1: Add cache import at top of `src/ingest/mod.rs`**

Add after the existing `use` block (after `use tracing::{error, info, warn};`):

```rust
use crate::cache::Cache;
```

- [ ] **Step 2: Update `run_ingest` return type and final return value**

Change the function signature from:

```rust
async fn run_ingest(
    cfg: &Config,
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<()> {
```

to:

```rust
async fn run_ingest(
    cfg: &Config,
    pool: Arc<PgPool>,
    source: Arc<FileSource>,
    tx_sem: Arc<tokio::sync::Semaphore>,
) -> anyhow::Result<usize> {
```

And change the final line of the function from:

```rust
    if ingested > 0 {
        info!("ingested {ingested} new game(s)");
    }
    Ok(())
```

to:

```rust
    if ingested > 0 {
        info!("ingested {ingested} new game(s)");
    }
    Ok(ingested)
```

- [ ] **Step 3: Update `ingest_loop` signature and add cache clearing**

Change the function signature from:

```rust
pub async fn ingest_loop(cfg: Arc<Config>, pool: Arc<PgPool>) {
```

to:

```rust
pub async fn ingest_loop(cfg: Arc<Config>, pool: Arc<PgPool>, cache: Arc<Cache>) {
```

Replace the three match blocks in `ingest_loop` that call `reingest_stale`, `reconcile_missing`, and `run_ingest`. The full updated body of `ingest_loop` (from the `match reingest_stale` line through the end of the function):

```rust
    match reingest_stale(pool.clone(), source.clone(), tx_sem.clone()).await {
        Ok(n) if n > 0 => {
            info!("re-ingested {n} stale game(s)");
            cache.clear();
        }
        Err(e) => error!("re-ingest of stale games failed: {e:#}"),
        _ => {}
    }

    match reconcile_missing(&cfg, pool.clone(), source.clone(), tx_sem.clone()).await {
        Ok(n) if n > 0 => {
            info!("reconciled {n} missing game(s)");
            cache.clear();
        }
        Err(e) => error!("reconciliation failed: {e:#}"),
        _ => {}
    }

    loop {
        match run_ingest(&cfg, pool.clone(), source.clone(), tx_sem.clone()).await {
            Ok(n) if n > 0 => {
                cache.clear();
            }
            Ok(_) => {}
            Err(e) => error!("ingest run failed: {e:#}"),
        }
        tokio::time::sleep(cfg.ingest_interval).await;
    }
```

- [ ] **Step 4: Verify it compiles (main.rs still has the call site error)**

```bash
cargo build 2>&1 | grep error | head -20
```

Expected: only `main.rs` errors (wrong args to `serve` and `ingest_loop`).

---

### Task 6: Update `src/main.rs`

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Update `src/main.rs`**

Full updated file:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    use std::io::IsTerminal;
    tracing_subscriber::fmt()
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();
    let cfg = wsb::config::Config::from_env()?;
    let pool = wsb::db::connect(&cfg.database_url).await?;
    wsb::db::migrate(&pool).await?;

    let cfg = std::sync::Arc::new(cfg);
    let pool = std::sync::Arc::new(pool);
    let cache = std::sync::Arc::new(wsb::cache::Cache::new());

    let ingest_cfg = cfg.clone();
    let ingest_pool = pool.clone();
    let ingest_cache = cache.clone();
    tokio::spawn(async move {
        wsb::ingest::ingest_loop(ingest_cfg, ingest_pool, ingest_cache).await;
    });

    wsb::web::serve(cfg, pool, cache).await
}
```

- [ ] **Step 2: Verify the full project compiles cleanly**

```bash
cargo build 2>&1 | grep -E '^error' | head -20
```

Expected: no errors.

- [ ] **Step 3: Run all tests**

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass. Look for `test result: ok` lines, no failures.

- [ ] **Step 4: cargo fmt**

```bash
cargo fmt
```

- [ ] **Step 5: Commit**

```bash
jj new -m "feat: wire HTTP response cache into web server and ingest loop"
```
