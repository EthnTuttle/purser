# Requirements Specification Document  
**Project:** Purser — Nostr-First Zaprite Replacement Daemon
**Version:** 1.3
**Date:** March 26, 2026
**Author:** Bitcoin Veterans
**Status:** Requirements (Waterfall Phase 1 – ready for Design & Implementation)

## 1. Project Overview & Objectives

### 1.1 Name: Purser
The daemon is named **Purser**, after the officer historically responsible for managing payments and provisions aboard ships. Dating back to at least the 13th century, the purser (from Anglo-French *bursier*, "keeper of the purse") served as the financial steward on naval and merchant vessels — handling all monetary transactions, maintaining accounts, and ensuring that goods were paid for and delivered. The role carried significant trust: the purser was personally accountable for every coin that passed through the ship's stores. The name reflects this daemon's purpose: a single, trusted agent that sits between your customers and your payment processors, accountable for every transaction that flows through it.

### 1.2 Objectives
Replace Zaprite entirely with a sovereign, Nostr-native checkout system that uses:
- A pluggable payment provider architecture. V1 ships with Square (fiat/cards) and Strike (Bitcoin/Lightning); additional providers can be added by implementing the `PaymentProvider` trait (Section 2.3).
- Nostr as the sole persistent data/communications layer (carts, orders, confirmations).
- A single lightweight daemon running on your always-on personal hardware (Raspberry Pi, mini-PC, VPS, etc.).

The daemon must never expose any public HTTP endpoints. All customer interaction happens via Nostr using the **marmot-protocol/mdk** (Marmot Development Kit) for encrypted messaging.

**Mandatory Nostr library:** https://github.com/marmot-protocol/mdk (mdk-core + mdk-memory-storage or mdk-sqlite-storage).  
All encrypted merchant↔customer communication (carts, orders, payment requests, status updates) MUST use MDK’s MLS-based protocol (treated as 1:1 checkout groups).

All payment provider API credentials stay 100% on your machine.

Success criteria:
- Private per-user carts (encrypted via MDK, merchant-viewable).
- NIP-15-derived checkout flow using MDK for all messaging (custom single-site protocol; not interoperable with standard NIP-15 marketplace clients).
- Payment confirmation events containing concrete, verifiable payment provider IDs.
- Zero polling unless a checkout is actively pending.
- Daemon runs 24/7 with minimal resources.

## 2. High-Level Architecture (Waterfall Design Constraint)

### 2.1 MDK Dependency Management
- MDK is currently alpha. Pin to a specific Git commit hash (to be determined before implementation begins). Do not track `main`.
- Before implementation begins, verify the following MDK capabilities work against the pinned commit:
  1. `MDK::new(...)` initializes with SQLite storage backend.
  2. `create_group` successfully establishes a 1:1 MLS group between two Nostr keypairs.
  3. `create_message` / `process_message` round-trip an encrypted payload through at least one relay.
  4. Key package generation and welcome messages complete without error.
- If any of the above fail, implementation is blocked until resolved upstream or a working commit is identified.

### 2.2 System Overview
- **Single executable daemon** written in **Rust**.
- **Nostr side:** One persistent outbound WebSocket connection to 2–5 relays (configurable). Use **marmot-protocol/mdk** (mdk-core) for all encrypted messaging, key packages, welcome messages, and group messages.
- **Payments side:** Pluggable payment providers via `PaymentProvider` trait (Section 2.3). V1 ships with Square and Strike implementations.
- **State:** In-memory + persisted as encrypted Nostr events via MDK storage (in-memory or SQLite). No local database or files except .env and logs.
- **Polling rule (critical):** Poll payment provider APIs **only** when one or more checkouts are “pending”. Stop polling immediately after confirmation or timeout. Each provider declares its own back-off schedule via `poll_config()`.
- **No public exposure:** No webhooks, no ngrok/Cloudflare Tunnel required (polling-only mode).

### 2.3 Payment Provider Trait

All payment processor integrations implement a common `PaymentProvider` trait. This allows new providers to be added without modifying core daemon logic.

```rust
trait PaymentProvider {
    /// Unique identifier, e.g. "square", "strike"
    fn name(&self) -> &str;

    /// Payment methods this provider handles, e.g. ["fiat"], ["lightning"]
    fn supported_methods(&self) -> Vec<PaymentMethod>;

    /// Create a payment or invoice from a validated order.
    /// Returns provider-specific payment details (URLs, invoices, etc.).
    async fn create_payment(&self, order: &ValidatedOrder) -> Result<ProviderPaymentRequest>;

    /// Check the current status of a previously created payment.
    async fn check_status(&self, payment_id: &str) -> Result<PaymentStatus>;

    /// Cancel or deactivate a payment (e.g. on expiry timeout).
    async fn cancel_payment(&self, payment_id: &str) -> Result<()>;

    /// Provider-specific polling configuration: initial interval,
    /// back-off schedule, rate limit budget, and header monitoring strategy.
    fn poll_config(&self) -> PollConfig;
}
```

- **V1 implementations:** `SquareProvider` and `StrikeProvider`.
- New providers implement this trait and are registered via `config.toml` (see Section 4).
- The daemon routes incoming orders to the appropriate provider by matching the order's `payment_method` against each provider's `supported_methods()`.
- Provider implementations are responsible for their own API authentication, request formatting, and response parsing.

## 3. Functional Requirements

### 3.1 Product Catalog
- The daemon loads a static product catalog from `products.toml` at startup.
- The catalog defines all purchasable items and services. Example structure:
```toml
[[products]]
id = "heltec-v3-single"
name = "Heltec V3 Meshtastic Device"
type = "physical"               # "physical" or "service"
price_usd = "59.99"
variants = ["single", "3-pack", "5-pack", "10-pack"]
active = true

[[products]]
id = "consulting-dev"
name = "Software Development Consulting"
type = "service"
price_usd = "0.00"              # 0.00 = custom quote required
variants = []
active = true
```
- On receiving an `order`, the daemon validates:
  - Every `product_id` exists in the catalog and is `active`.
  - The `unit_price_usd` matches the catalog price (unless catalog price is `"0.00"`, indicating custom quote).
  - The `variant` (if provided) is in the product's `variants` list.
- Invalid or mismatched orders are rejected with an error via MDK group message.
- Custom-quote products (`price_usd = "0.00"`) require the merchant to manually approve and set the price. The daemon holds the order in a `pending_quote` state and notifies the merchant (see Section 3.9).
- Changes to `products.toml` require a daemon restart to take effect.

### 3.2 Nostr Communication (WebSocket + MDK)
- Connect once at startup to user-configured relays using rust-nostr (integrated by MDK).
- Use **mdk-core** exclusively for all encrypted communication:
  - For each new cart/checkout session, create a temporary 1:1 MLS group (merchant pubkey + customer pubkey).
  - Customer sends `order` event (NIP-15-derived, see message schemas in Section 3.3) as an MDK-encrypted group message.
  - Merchant replies with `payment-request` event as an MDK-encrypted group message (containing provider-specific payment details — see `payment_details` in Section 3.3.2).
  - On payment completion, merchant sends `status-update` event as an MDK-encrypted group message.
- Carts: Store live carts as addressable events (kind 30078) or series of MDK-encrypted group messages tagged with `d:cart:USER_PUBKEY:SESSION_ID`. Merchant can decrypt everything via MDK.
- Publish signed confirmation events (kind 1 or custom status kind) containing:
  - `order_id`, `payment_provider` (provider name string),
  - `payment_id`,
  - `amount`, `currency`, `status:PAID`, `timestamp`,
  - optional Lightning preimage.
- All events and messages handled via MDK’s `MDK::new(...)`, `create_group`, `create_message`, `process_message`, `create_key_package_for_event`, etc.
- Automatic key-package generation, welcome messages, and rotation handled by MDK.

### 3.3 Message Schemas (MDK Group Message Payloads)

All payloads are JSON, sent as the body of MDK-encrypted group messages. Each message includes a `type` field to distinguish message kinds and a `version` field for protocol versioning. The daemon rejects messages with an unrecognized `version`.

#### 3.3.1 `order` (customer → merchant)
```json
{
  "version": 1,
  "type": "order",
  "order_id": "<client-generated UUIDv4>",
  "items": [
    {
      "product_id": "<string>",
      "name": "<string>",
      "quantity": "<int>",
      "variant": "<string, optional — e.g. '5-pack', 'XL', 'blue'>",
      "unit_price_usd": "<decimal as string, e.g. '59.99'>"
    }
  ],
  "order_type": "physical | service",
  "shipping": {
    "name": "<string>",
    "address_line_1": "<string>",
    "address_line_2": "<string, optional>",
    "city": "<string>",
    "state": "<string>",
    "zip": "<string>",
    "country": "<ISO 3166-1 alpha-2>"
  },
  "service_details": {
    "description": "<string>",
    "preferred_date": "<ISO 8601, optional>",
    "notes": "<string, optional>"
  },
  "contact": "<email or nostr npub, optional>",
  "payment_method": "<string — e.g. 'fiat', 'lightning', 'onchain', 'ecash'>",
  "currency": "USD",
  "message": "<optional free-text note to merchant>"
}
```
- `shipping` is required when `order_type` = `"physical"`.
- `service_details` is required when `order_type` = `"service"`.
- `order_id` is client-generated and serves as an idempotency key — the daemon must reject duplicate `order_id` values from the same customer pubkey.
- Prices are strings to avoid floating-point issues.

#### 3.3.2 `payment-request` (merchant → customer)
```json
{
  "version": 1,
  "type": "payment-request",
  "order_id": "<echoed from order>",
  "payment_provider": "<string — provider name, e.g. 'square', 'strike'>",
  "payment_id": "<string — provider's external payment/invoice ID>",
  "payment_details": {
    // Provider-specific fields. Examples:
    // Square: { "checkout_url": "https://square.link/u/..." }
    // Strike: { "lightning_invoice": "lnbc1..." }
    // Future:  { "btcpay_url": "...", "onchain_address": "bc1..." }
  },
  "amount": "<decimal as string>",
  "currency": "<string — e.g. 'USD', 'BTC', 'sats'>",
  "expires_at": "<ISO 8601 timestamp>"
}
```

#### 3.3.3 `status-update` (merchant → customer)
```json
{
  "version": 1,
  "type": "status-update",
  "order_id": "<string>",
  "status": "paid | shipped | failed | expired | refunded",
  "payment_provider": "<string — provider name>",
  "payment_id": "<string>",
  "amount": "<decimal as string>",
  "currency": "<string — e.g. 'USD', 'BTC', 'sats'>",
  "timestamp": "<ISO 8601>",
  "lightning_preimage": "<string, optional>",
  "tracking": {
    "carrier": "<string>",
    "tracking_number": "<string>"
  },
  "message": "<optional merchant note>"
}
```
- `tracking` is optional, included when `status` = `"shipped"` for physical goods.

### 3.4 Payment Creation
- On receiving valid `order` message via MDK-encrypted group message:
  - Validate message against schema (Section 3.3.1) and product catalog (Section 3.1). Reject if required fields are missing or `order_id` is a duplicate.
  - Parse order JSON.
  - Route to the appropriate `PaymentProvider` implementation based on the order's `payment_method` field matched against each provider's `supported_methods()`.
  - Call `provider.create_payment(order)` to create the payment/invoice with the external provider.
  - Store pending payment record (in-memory + encrypted via MDK) with internal `order_id`, provider name, external `payment_id`, expiry, etc.
  - Reply immediately with `payment-request` MDK group message containing provider-populated `payment_details`.

### 3.5 Smart Conditional Polling
- Maintain an in-memory set of “pending” payment records, keyed by `order_id`, storing the provider, external payment/invoice ID, and creation timestamp.
- **Only when the pending set is non-empty:**
  - Each provider declares its polling behavior via `poll_config()`: initial interval, back-off schedule, and rate limit strategy.
  - **Default back-off schedule** (providers may override): initial poll at 10s, then 30s → 60s → 120s → max 5 min.
  - Per-payment timer — each pending payment tracks its own back-off independently.
  - One `provider.check_status(payment_id)` call per pending payment per cycle.
  - **V1 provider poll configs:**
    - **SquareProvider:** `GetPayment` per payment per cycle. Monitors `X-RateLimit-Remaining` header — doubles interval if below 20% of limit. Known limit: ~10 req/sec (unpublished, use header monitoring).
    - **StrikeProvider:** `Find Invoice by ID` per invoice per cycle. Known limit: 1,000 req/10 min for non-creation endpoints.
  - On `COMPLETED` / `PAID`:
    - Publish Nostr confirmation via MDK-encrypted group message (see 3.2).
    - Remove from pending set.
    - (Optional) send NIP-57 zap to customer.
- **Expiry:** On payment expiry (per `expires_at` set in Section 3.6), mark as failed, send `status-update` with `status: “expired”`, and remove from pending set.
- **Idle:** No polling timer when the pending set is empty — zero API calls when no checkouts are in flight.

### 3.6 Payment Edge Cases
- **Duplicate orders:** If an `order` message arrives with an `order_id` already seen from the same customer pubkey, return the existing `payment-request` rather than creating a new payment. This handles client retry on flaky connections.
- **Partial / overpayments:** Accept payments within a configurable margin of error (default: ±2%). Log the discrepancy. Treat within-margin as paid. (Primarily relevant for Lightning providers; fiat providers handle exact amounts.)
- **Refunds:** Out of scope for v1. Merchant handles refunds directly via each provider’s dashboard.
- **Currency / exchange rate:** All prices are in USD. Providers that accept non-USD payment (e.g. Strike for Lightning) handle conversion at invoice creation time. Exchange rate risk is bounded by the provider’s invoice expiry window. No daemon-side conversion logic.
- **Payment expiry alignment:**
  - Each provider’s `create_payment()` returns an `expires_at` timestamp. If the provider doesn’t natively expire payments (e.g. Square payment links), the implementation sets a daemon-managed timeout (configurable, default 15 minutes) and calls `cancel_payment()` on expiry.
  - On expiry: daemon sends `status-update` with `status: "expired"` via MDK group message and removes from pending set.
- **Provider selection:** The daemon matches the order’s `payment_method` field against each registered provider’s `supported_methods()`. If no provider supports the requested method, the order is rejected with an error. Provider-to-method mapping is configured in `config.toml`.

### 3.7 Error Handling & Recovery
- Invalid MDK message → silent ignore or polite error reply via MDK group message.
- API rate limits / transient errors → retry with back-off, never crash daemon.
- Relay disconnect → auto-reconnect with exponential back-off (handled by rust-nostr + MDK).
- Daemon restart → recover pending payments and group states using MDK’s persistent storage (SQLite option).

### 3.8 Anti-Spam / Rate Limiting
- **Per-pubkey rate limits:** The daemon tracks order activity per customer pubkey using an in-memory sliding window.
  - Max 1 pending checkout session per pubkey (enforced in Section 3.8 concurrent sessions rule).
  - Max 10 order attempts per pubkey per hour. Attempts beyond this are silently dropped.
  - Max 3 failed/expired orders per pubkey per 24 hours. Beyond this, the pubkey is temporarily blocked for 24 hours.
- Rate limit counters are in-memory only — they reset on daemon restart (acceptable trade-off for simplicity).
- Blocked pubkeys receive a single error message via MDK group message indicating temporary rate limit, then further messages from that pubkey are ignored until the block expires.
- All rate limit thresholds are configurable via `config.toml`.

### 3.9 Nostr/MDK Edge Cases
- **Customer offline at confirmation:** Rely on relay message persistence. The daemon publishes the `status-update` to relays via MDK and does not retry. The customer’s client retrieves it when it reconnects.
- **Concurrent sessions per customer:** One active checkout session per customer pubkey. If a customer sends a new `order` while a previous session is still pending, reject the new order with an error message referencing the existing session.
- **MLS key package exhaustion:** MDK handles key package generation and rotation internally. The daemon must call MDK’s key package generation on startup and periodically (interval configurable, default every 6 hours) to ensure availability.
- **Stale MLS group cleanup:** MLS groups created for checkout sessions are ephemeral. After a `status-update` of `paid`, `failed`, `expired`, or `refunded` is sent, the daemon marks the group as inactive. MDK’s storage backend handles cleanup of inactive groups. The daemon should not accumulate unbounded group state — purge groups older than 7 days (configurable) on a periodic sweep.

### 3.10 Daemon Lifecycle
- **Graceful shutdown:** On SIGTERM/SIGINT, stop accepting new orders, allow in-progress API calls to complete (with a 10-second hard deadline), persist the pending payment set to SQLite, then exit.
- **Startup recovery:** On restart, reload the pending payment set from SQLite and resume polling for any payments that have not expired. Re-establish MDK group state from persistent storage. Log the number of recovered pending payments at startup.
- **Clock handling:** Use monotonic timers (e.g. `tokio::time::Instant`) for all polling intervals and back-off calculations — never wall clock. Timestamps in outgoing messages use UTC wall clock (`chrono::Utc::now()`).
- **Disk full / SQLite errors:** If SQLite writes fail, log an error and continue operating with in-memory state only — do not crash. Log a warning when SQLite DB size exceeds a configurable threshold (default: 500 MB).

## 4. Non-Functional Requirements
- **Security:** API keys and Nostr private key NEVER in code or git. Use .env + OS secret manager where possible. All encryption handled by MDK (MLS). Validate all incoming MDK messages. No customer data stored unencrypted.
- **Performance:** < 200 ms daemon processing time for order → payment-request MDK message (excludes external API round-trip to provider). Daemon < 150 MB RAM (profile under load with concurrent MLS groups, SQLite, and WebSocket buffers), < 5 % CPU idle.
- **Reliability:** Auto-restart via systemd / launchd / Docker. Logs to stdout + optional file.
- **Observability:** Simple console logs + optional MDK status reports to merchant.
- **Configurability:** relays list, payment provider configs (see below), merchant Nostr keypair, polling intervals, expiry times, MDK storage type — all via .env or config.toml. Provider configuration uses a `[[providers]]` array in `config.toml`:
  ```toml
  [[providers]]
  type = "square"
  methods = ["fiat"]
  api_key_env = "SQUARE_API_KEY"    # references .env variable

  [[providers]]
  type = "strike"
  methods = ["lightning"]
  api_key_env = "STRIKE_API_KEY"    # references .env variable
  ```
- **Language:** **Rust only** (required by mdk-core). Use latest stable Rust.

## 5. Deliverables

1. **System architecture diagram** — Mermaid or ASCII showing daemon, relay connections, payment provider trait boundary, and MDK message paths.
2. **Project structure** — Full folder layout with `Cargo.toml`, `src/` modules, config files.
3. **Daemon source code** — Complete, compilable Rust crate using mdk-core, reqwest (for provider REST calls), and rust-nostr where needed. Includes `SquareProvider` and `StrikeProvider` implementations.
4. **Configuration templates** — `.env.example` and `config.toml.example` covering all configurable values from this spec.
5. **Product catalog template** — `products.toml.example` with sample entries for physical goods and services.
6. **Deployment guide** — Step-by-step instructions for Raspberry Pi / Linux (systemd unit file) and Docker (Dockerfile + compose).
7. **Test plan** — Unit tests for message schema validation, payment creation, and polling logic. End-to-end test with a sample Nostr client that sends MDK-encrypted order messages and verifies the full checkout flow.
8. **Security checklist** — Threat model summary covering key management, MLS encryption advantages, API credential handling, and anti-spam protections.

## 6. Implementation Constraints

- NIP-15-derived logical flow (order → payment-request → status-update). All messages use MDK-encrypted MLS groups, not standard NIP-15 event kinds. Standard NIP-15 client interoperability is not a goal for v1.
- All encrypted messaging MUST use marmot-protocol/mdk. No raw NIP-44 or NIP-04.
- Smart conditional polling only — never poll when idle (Section 3.5).
- All payment provider integrations via official REST endpoints only. No webhooks. V1 providers: Square and Strike.
- All Nostr events and messages routed through MDK.
- Minimal external dependencies: mdk-core, reqwest, rust-nostr (via MDK), serde, tokio, chrono. No unnecessary crates.

## 7. Acceptance Criteria

Each functional requirement maps to one or more testable acceptance criteria. Tests are categorized as **Unit** (no external services), **Integration** (requires relays and/or sandbox APIs), or **E2E** (full flow with test Nostr client).

| # | Requirement | Test type | Test description | Pass condition |
|---|---|---|---|---|
| 1 | 3.1 Catalog — valid load | Unit | Start daemon with well-formed `products.toml` | Daemon starts, logs product count, all products queryable in memory |
| 2 | 3.1 Catalog — invalid file | Unit | Start daemon with malformed `products.toml` (missing required field) | Daemon logs error and exits with non-zero status |
| 3 | 3.1 Catalog — order validation | Unit | Submit order with unknown `product_id` | Order rejected with error message via MDK |
| 4 | 3.1 Catalog — price mismatch | Unit | Submit order where `unit_price_usd` differs from catalog | Order rejected with error message via MDK |
| 5 | 3.1 Catalog — custom quote | Unit | Submit order for product with `price_usd = "0.00"` | Order held in `pending_quote` state, merchant notified |
| 6 | 3.2 Nostr — relay connection | Integration | Start daemon with 3 configured relays | Daemon connects to all 3, logs successful connection |
| 7 | 3.2 Nostr — relay disconnect | Integration | Kill one relay mid-session | Daemon logs disconnect, auto-reconnects with back-off, continues operating on remaining relays |
| 8 | 3.2 Nostr — MLS group creation | Integration | Customer pubkey sends first message | Daemon creates 1:1 MLS group, sends welcome message via MDK |
| 9 | 3.3 Schema — valid order | Unit | Send well-formed `order` JSON matching Section 3.3.1 | Parsed without error, all fields accessible |
| 10 | 3.3 Schema — missing required field | Unit | Send `order` JSON without `order_id` | Rejected with schema validation error |
| 11 | 3.3 Schema — physical without shipping | Unit | Send `order` with `order_type: "physical"` and no `shipping` block | Rejected with error specifying missing shipping |
| 12 | 3.3 Schema — service without service_details | Unit | Send `order` with `order_type: "service"` and no `service_details` block | Rejected with error specifying missing service details |
| 13 | 2.3 Provider — trait contract | Unit | Implement a mock `PaymentProvider`, register in config | Daemon loads mock provider, routes orders to it by `payment_method` |
| 14 | 2.3 Provider — unknown type | Unit | Configure a provider with `type = "nonexistent"` | Daemon logs error and exits with non-zero status |
| 15 | 2.3 Provider — unsupported method | Unit | Send order with `payment_method` no provider handles | Order rejected with error specifying unsupported payment method |
| 16 | 3.4 Payment — Square creation | Integration | Send valid order with `payment_method: "fiat"` | `SquareProvider.create_payment()` called, `payment-request` returned with `payment_details.checkout_url` and valid `expires_at` |
| 17 | 3.4 Payment — Strike creation | Integration | Send valid order with `payment_method: "lightning"` | `StrikeProvider.create_payment()` called, `payment-request` returned with `payment_details.lightning_invoice` and valid `expires_at` |
| 18 | 3.4 Payment — processing time | Integration | Measure time from order receipt to `payment-request` send | Daemon processing time < 200ms (excluding external API round-trip) |
| 19 | 3.5 Polling — initial poll timing | Integration | Create a pending payment, mock provider returns `COMPLETED` | First poll fires at 10s, `status-update` with `status: "paid"` sent within that cycle |
| 20 | 3.5 Polling — back-off | Integration | Create a pending payment, mock provider returns `PENDING` for 3 cycles | Poll intervals follow provider's `poll_config()` schedule |
| 21 | 3.5 Polling — idle state | Integration | No pending payments | Zero `check_status()` calls over a 5-minute window |
| 22 | 3.5 Polling — rate limit response | Integration | Mock provider rate limit signal (e.g. Square `X-RateLimit-Remaining` below 20%) | Daemon doubles polling interval for next cycle per provider's `poll_config()` strategy |
| 23 | 3.6 Payment edge — duplicate order | Integration | Send same `order_id` from same pubkey twice | Second request returns the existing `payment-request`, no new payment created |
| 24 | 3.6 Payment edge — partial payment | Integration | Provider reports payment at 99% of amount (within ±2%) | Treated as paid, `status-update` sent with `status: "paid"`, discrepancy logged |
| 25 | 3.6 Payment edge — expiry | Integration | Let payment exceed `expires_at` without completion | `provider.cancel_payment()` called, `status-update` with `status: "expired"` sent, removed from pending set |
| 26 | 3.7 Error — invalid MDK message | Integration | Send garbage data as MDK group message | Daemon does not crash, logs warning, ignores message |
| 27 | 3.7 Error — provider API down | Integration | Mock provider returning 500 errors | Daemon retries with back-off, does not crash, logs errors |
| 28 | 3.8 Anti-spam — rate limit | Integration | Send 11 order attempts from same pubkey within 1 hour | First 10 accepted, 11th silently dropped |
| 29 | 3.8 Anti-spam — failure block | Integration | Send 4 orders from same pubkey that all expire within 24h | 4th order triggers 24h block, pubkey receives rate-limit error |
| 30 | 3.9 Nostr edge — customer offline | Integration | Send `status-update`, customer client disconnected | Message published to relays. Customer retrieves it on reconnect |
| 31 | 3.9 Nostr edge — concurrent session | Integration | Send new `order` while previous session is pending | New order rejected with error referencing existing session |
| 32 | 3.10 Lifecycle — graceful shutdown | Integration | Send SIGTERM with 2 pending payments | In-progress API calls complete (or hit 10s deadline), pending set persisted to SQLite |
| 33 | 3.10 Lifecycle — startup recovery | Integration | Restart daemon after graceful shutdown from test #32 | Daemon reloads 2 pending payments from SQLite, resumes polling, logs recovery count |
| 34 | 3.10 Lifecycle — disk full | Integration | Mock SQLite write failure | Daemon logs error, continues operating in-memory, does not crash |
| 35 | E2E — full checkout (fiat) | E2E | Test client sends order (`payment_method: "fiat"`), pays via Square sandbox | Full flow: order → payment-request → payment → status-update with `status: "paid"` and valid `payment_id` |
| 36 | E2E — full checkout (lightning) | E2E | Test client sends order (`payment_method: "lightning"`), pays via Strike sandbox | Full flow: order → payment-request → invoice payment → status-update with `status: "paid"` and valid `payment_id` |
| 37 | E2E — full checkout with shipping | E2E | Complete test #35, then merchant sends shipping update | `status-update` with `status: "shipped"` includes `tracking` object |
