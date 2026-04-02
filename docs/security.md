# Purser Security Checklist

## 1. Key Management

- **Merchant Nostr private key** is stored only in `.env`, never in `config.toml` or source code. The daemon reads it via `std::env::var` at startup.
- **Payment provider API keys** (Square, Strike) are referenced by environment variable name in `config.toml` (e.g., `api_key_env = "SQUARE_API_KEY"`). The actual secret values live exclusively in `.env`.
- **`.env` must be excluded from version control.** The `.gitignore` file must contain an entry for `.env`. Committing secrets is a critical security failure.
- **systemd deployment** uses `EnvironmentFile=/path/to/.env` in the unit file to inject secrets into the daemon process without exposing them in the process command line or service definition.
- **Docker deployment** uses `env_file: .env` in `docker-compose.yml` to inject secrets. The `.env` file must not be baked into the image (it is excluded via `.dockerignore`).

## 2. MLS Encryption (via MDK)

- **All customer-merchant communication is encrypted** via MLS 1:1 groups managed by the MDK library. Each checkout session creates a dedicated MLS group between the merchant and the customer.
- **No raw NIP-44 or NIP-04 encryption is used.** The daemon never implements or calls low-level Nostr encryption primitives. MDK handles all key exchange, encryption, and decryption.
- **Key package rotation** is configurable (default: every 6 hours). The `NostrClient::regenerate_key_packages` method handles this. Fresh key packages ensure that new sessions use current keying material.
- **Stale group cleanup** is configurable (default: after 7 days of inactivity). The `NostrClient::purge_stale_groups` method removes old MLS groups, limiting the window of exposure for completed sessions.
- **Forward secrecy** is provided by the MLS protocol. Compromise of current keys does not reveal past session content.

## 3. API Credential Handling

- **Credentials are never logged.** The tracing configuration must not include API keys, tokens, or other secrets in log output. Provider implementations must take care not to pass credentials to `tracing::info!`, `tracing::debug!`, or similar macros.
- **HTTPS-only connections to provider APIs.** The `reqwest` HTTP client enforces TLS by default. Provider base URLs use `https://` exclusively (`https://connect.squareup.com`, `https://api.strike.me`). Plain HTTP is never used.
- **API keys are loaded once at startup** via `init_providers` in `main.rs`. They are held in memory within the provider structs for the lifetime of the daemon. They are not re-read from the environment, written to disk, or transmitted over the network (except as Authorization headers to their respective APIs over HTTPS).

## 4. Anti-Spam Protections

- **Per-pubkey rate limiting:** The `RateLimiter` enforces a configurable maximum number of order attempts per hour (default: 10). Excess attempts are rejected before reaching the payment provider.
- **Failure block:** A configurable failure threshold (default: 3 failures within 24 hours) triggers a configurable block duration (default: 24 hours). During the block, all orders from that pubkey are rejected.
- **One concurrent session per pubkey.** The `RateLimiter` tracks active checkout sessions via an `active_sessions` set. A customer cannot open a second checkout while one is already pending.
- **Rate limit counters reset on daemon restart.** This is a documented trade-off: simplicity and no persistence dependency for rate-limit state. An attacker could reset limits by triggering a daemon restart, but this requires access to the host machine itself.

## 5. Input Validation

- **Schema validation:** All incoming order messages are validated against the expected JSON schema (version field, message type, required fields). Malformed messages are rejected before any processing occurs.
- **Catalog validation:** Every order item is checked against the product catalog loaded from `products.toml`. The product must exist, be marked active, and the submitted price must match the catalog price. Orders referencing unknown or inactive products are rejected.
- **Duplicate order_id detection:** The `is_duplicate_order` check ensures idempotency. If an order_id already exists in the pending payments map, the duplicate is rejected to prevent double-charging.
- **Garbage resilience:** Malformed or unparseable messages are silently dropped (no crash). They are logged at `warn` level for operator visibility but do not affect daemon stability or other sessions.

## 6. Attack Surface

- **No public HTTP endpoints.** The daemon opens zero inbound network listeners. There is no HTTP server, no webhook receiver, no API endpoint exposed to the internet.
- **Payment status is checked via outbound polling only.** The `PollingEngine` makes outbound HTTPS requests to provider APIs. No inbound callbacks are needed or accepted.
- **No webhooks, no ngrok/tunnel required.** The polling-only architecture eliminates an entire class of attacks (webhook spoofing, SSRF via callback URLs, tunnel hijacking).
- **Only outbound connections are made:**
  - Nostr relays via WSS (WebSocket Secure)
  - Square API via HTTPS
  - Strike API via HTTPS
- No other network communication occurs. DNS resolution is the only additional outbound traffic.

## 7. SQLite Security

- **Database file permissions should be `0600`** (owner read/write only). The deployment guide recommends setting this explicitly. The SQLite database contains pending payment metadata (order IDs, amounts, customer pubkeys) which should not be world-readable.
- **Disk-full resilience:** If SQLite writes fail (e.g., disk full), the daemon continues operating with in-memory state. Pending payment persistence is best-effort -- the daemon logs errors but does not crash. On next clean shutdown, it will attempt to persist again.
- **Configurable size warning threshold** (default: 500 MB). Operators are alerted via log warnings if the database grows unexpectedly, which could indicate a bug or abuse.

## 8. Supply Chain

- **MDK is pinned to a specific commit hash** (`adaf261ba8f9aca8e5c3049bc85ac236837af76c`) in `Cargo.toml`. The daemon does not track the MDK `main` branch. Upgrades are deliberate and require verification.
- **`Cargo.lock` is committed** to the repository for reproducible builds. Every build resolves the exact same dependency versions.
- **Minimal dependency set.** The daemon uses only essential crates: `mdk-core` (Nostr/MLS), `tokio` (async runtime), `reqwest` (HTTP client), `serde`/`serde_json` (serialization), `chrono` (timestamps), `rusqlite` (persistence), `tracing` (logging). No unnecessary or convenience crates are included.
