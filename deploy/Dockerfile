# Stage 1: Build
FROM rust:1.82-bookworm AS builder

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src/ src/

# rusqlite bundled feature compiles SQLite from source, no system lib needed
RUN cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd --system --uid 1000 --no-create-home purser

# Config and data directories
RUN mkdir -p /etc/purser /var/lib/purser \
    && chown purser:purser /var/lib/purser

COPY --from=builder /build/target/release/purser /usr/local/bin/purser

USER purser

VOLUME ["/etc/purser", "/var/lib/purser"]

ENTRYPOINT ["/usr/local/bin/purser"]
