FROM rust:1.95-slim-bookworm AS builder

WORKDIR /app

# Build dependencies in an isolated layer so they are only re-compiled when
# Cargo.toml or Cargo.lock change, not on every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && \
    printf 'pub fn _stub(){}' > src/lib.rs && \
    printf 'fn main(){}' > src/main.rs && \
    SQLX_OFFLINE=true cargo build --release --locked && \
    rm -rf src \
           target/release/.fingerprint/wsb-* \
           target/release/deps/libwsb-* \
           target/release/wsb

# Copy application source. Migrations and templates are embedded at compile time
# by sqlx::migrate! and include_str!, so they must be present during the build.
COPY src ./src
COPY migrations ./migrations
COPY templates ./templates
COPY .sqlx ./.sqlx

ENV SQLX_OFFLINE=true
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/wsb /usr/local/bin/wsb
COPY --from=builder /app/target/release/backfill_from_zip /usr/local/bin/backfill_from_zip

EXPOSE 8080
CMD ["wsb"]
