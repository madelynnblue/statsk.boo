# HTTP Cache Middleware Design

**Date:** 2026-05-14  
**Status:** Approved

## Overview

Add an in-memory HTTP response cache as Axum middleware. All routes except `/search` are cached by URL path + query string. Cached bytes are compressed with zstd. The cache is cleared after any ingest pass that writes one or more games.

## Cache Struct (`src/web/cache.rs`)

```rust
struct CachedEntry {
    headers: HeaderMap,  // original response headers
    body: Vec<u8>,       // zstd-compressed body bytes
}

struct Cache(RwLock<HashMap<String, Arc<CachedEntry>>>);
```

Methods:
- `get(key: &str) -> Option<Arc<CachedEntry>>` — read lock, clone Arc out, release
- `set(key: String, entry: Arc<CachedEntry>)` — write lock, insert, release
- `clear()` — write lock, `HashMap::clear()`, release

Uses `std::sync::RwLock` (not `tokio`). No `.await` is held while the lock is held, so this is safe in async context. `Arc<CachedEntry>` makes cache hits a pointer clone, not a byte copy.

## Middleware (`cache_middleware` in `src/web/cache.rs`)

Registered with `axum::middleware::from_fn_with_state` on the full router.

```
1. path == "/search" → next.run(req) and return (no caching)
2. key = req.uri().to_string()
3. cache.get(&key):
   HIT  → zstd::decode_all(entry.body), reconstruct response from entry.headers, return
   MISS → next.run(req), collect body with axum::body::to_bytes
          status != 200 OK → return as-is, do not cache
          status == 200 OK → zstd::encode_all(body, level=3),
                              cache.set(key, CachedEntry { headers, body }),
                              return response from original headers + uncompressed bytes
```

Only `200 OK` responses are cached. Non-200s (404, 500, etc.) pass through uncached so they don't get stuck across ingests.

## Wiring

### `src/main.rs`
- Construct `Arc<Cache>` once
- Pass a clone to the ingest task and a clone to `serve`

### `src/web/mod.rs`
- `serve` accepts `Arc<Cache>` as a new parameter
- Add `cache: Arc<Cache>` to `AppState`
- Apply `axum::middleware::from_fn_with_state(state.clone(), cache_middleware)` to the router

### `src/ingest/mod.rs`
- `ingest_loop` gains `cache: Arc<Cache>` parameter
- `run_ingest` return type changes from `anyhow::Result<()>` to `anyhow::Result<usize>` (already computes `ingested` count; just return it)
- Three clear sites, each conditional on `n > 0`:

```rust
// startup
reingest_stale(...)     → Ok(n) if n > 0 → cache.clear()
reconcile_missing(...)  → Ok(n) if n > 0 → cache.clear()

// periodic loop
run_ingest(...)         → Ok(n) if n > 0 → cache.clear()
```

## Dependencies

```
cargo add zstd
```

zstd level 3 is the default encode level — good ratio (~3–4× for HTML) with fast decode on cache hits.

## What is NOT cached
- `/search` — excluded entirely; results depend on unbounded query strings and should never be cached
- Non-200 responses — pass through uncached
- No TTL, no max-size limit — cache lifetime is bounded by ingest frequency; unbounded growth is not a concern for this workload (O(unique URLs) entries of 5–50 KB compressed)
