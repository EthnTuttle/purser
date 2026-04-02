# Purser Architecture

## System Diagram

```mermaid
graph TB
    subgraph Customer Side
        Customer["Customer<br/>(Nostr Client)"]
    end

    subgraph Nostr Network
        R1["Relay 1<br/>(WSS)"]
        R2["Relay 2<br/>(WSS)"]
        R3["Relay 3..5<br/>(WSS)"]
    end

    subgraph Purser Daemon
        NC["NostrClient"]
        MDK["MDK Encryption Layer<br/>(MLS 1:1 Groups)"]
        PL["Pipeline<br/>(process_order / handle_polling_event)"]
        RL["RateLimiter<br/>(per-pubkey sliding window)"]
        PE["PollingEngine<br/>(backoff + expiry)"]
        PS["PersistenceStore<br/>(SQLite)"]
        AS["AppState<br/>(in-memory pending payments)"]
    end

    subgraph Configuration
        CT["config.toml"]
        PT["products.toml"]
        ENV[".env<br/>(secrets)"]
    end

    subgraph PaymentProvider Trait Boundary
        PP["PaymentProvider trait"]
        SQ["SquareProvider"]
        ST["StrikeProvider"]
    end

    subgraph External APIs
        SQA["Square REST API<br/>(HTTPS)"]
        STA["Strike REST API<br/>(HTTPS)"]
    end

    Customer <-->|"Nostr messages"| R1
    Customer <-->|"Nostr messages"| R2
    Customer <-->|"Nostr messages"| R3

    R1 <-->|"WebSocket"| NC
    R2 <-->|"WebSocket"| NC
    R3 <-->|"WebSocket"| NC

    NC <--> MDK
    MDK -->|"decrypted order JSON"| PL
    PL -->|"payment-request / status-update"| MDK

    PL -->|"check_allowed"| RL
    PL -->|"create_payment"| PP
    PL -->|"register payment"| PE
    PL -->|"read/write pending"| AS

    PE -->|"check_status"| PP
    PE -->|"PollingEvent"| PL

    PP --> SQ
    PP --> ST
    SQ -->|"HTTPS"| SQA
    ST -->|"HTTPS"| STA

    PS <-->|"save/load pending"| AS

    CT -->|"load at startup"| AS
    PT -->|"load at startup"| AS
    ENV -->|"secrets at startup"| AS
```

## Checkout Message Flow

```mermaid
sequenceDiagram
    participant C as Customer
    participant R as Nostr Relays
    participant N as NostrClient + MDK
    participant P as Pipeline
    participant RL as RateLimiter
    participant PP as PaymentProvider
    participant PE as PollingEngine

    C->>R: Encrypted order message
    R->>N: Forward to daemon
    N->>N: MDK decrypt (MLS group)
    N->>P: Raw order JSON + customer pubkey
    P->>RL: check_allowed(pubkey)
    RL-->>P: allowed / blocked
    P->>P: validate_order (schema + catalog)
    P->>P: is_duplicate_order check
    P->>PP: create_payment(validated_order)
    PP-->>P: ProviderPaymentRequest (payment_id, details, expiry)
    P->>PE: register(pending_payment)
    P->>N: Send payment-request via MDK
    N->>R: Encrypted payment-request
    R->>C: Forward to customer

    loop Polling (backoff intervals)
        PE->>PP: check_status(payment_id)
        PP-->>PE: PaymentStatus
    end

    PE->>P: PollingEvent::Completed
    P->>N: Send status-update (paid) via MDK
    N->>R: Encrypted status-update
    R->>C: Forward to customer
    P->>PE: Deregister payment
    P->>N: Deactivate MLS group
```

## System Overview

Purser is a single-executable Rust daemon that replaces Zaprite with a sovereign, Nostr-native checkout system. It connects outbound to 2-5 Nostr relays via WebSocket and to payment provider APIs (Square, Strike) via HTTPS. There are no inbound network listeners -- the daemon communicates with customers exclusively through MLS-encrypted Nostr messages handled by the MDK library, and checks payment status through outbound polling only. Configuration is loaded from `config.toml` and `products.toml` at startup, with secrets (API keys, Nostr private key) injected via environment variables from a `.env` file.

## Module Responsibilities

### NostrClient and MDK

The `NostrClient` struct wraps the MDK library and manages all Nostr communication. It maintains WebSocket connections to configured relays and delegates encryption, decryption, key package management, and MLS group lifecycle to MDK. Every customer interaction (orders in, payment-requests out, status-updates out) passes through this layer, ensuring end-to-end encryption via MLS 1:1 groups. The client also handles key package rotation and stale group cleanup on configurable schedules.

### Pipeline

The pipeline module (`pipeline.rs`) is the central orchestrator, exposing two top-level async functions. `process_order` handles the inbound path: it decrypts an incoming order via the NostrClient, checks rate limits, validates the order against the schema and product catalog, routes it to the appropriate payment provider, registers the resulting payment for polling, and sends a `payment-request` back to the customer. `handle_polling_event` handles the outbound path: when the polling engine emits a terminal event (completed, expired, or failed), it builds a `status-update` message and sends it to the customer, then cleans up the pending payment and deactivates the MLS group.

### PollingEngine

The `PollingEngine` drives payment status checks for all pending payments. It maintains an internal map of `PendingPollEntry` records, each tracking its provider reference, current backoff interval, and last poll timestamp. The engine runs a continuous loop: for each entry whose interval has elapsed, it calls `check_status` on the appropriate provider. If the status is terminal (completed, expired, failed), it emits a `PollingEvent` on an mpsc channel consumed by the event handler in `main.rs`. Backoff intervals are configured per-provider via `PollConfig` (initial interval, multiplier, max interval). The engine does zero work when there are no pending payments.

### RateLimiter

The `RateLimiter` provides per-pubkey anti-spam protection using in-memory sliding windows. It enforces three rules: a maximum number of order attempts per hour, a failure threshold that triggers a temporary block, and a one-concurrent-session-per-pubkey limit. All state is held in `Mutex`-protected `HashMap` structures and resets on daemon restart -- a documented trade-off that favors simplicity and avoids persisting rate-limit state to disk.

### PersistenceStore

The `PersistenceStore` wraps a SQLite database with a single `pending_payments` table. Its purpose is crash recovery: on shutdown, all in-flight pending payments are serialized to JSON and saved; on startup, they are loaded back and re-registered with the polling engine (skipping any that have expired while the daemon was down). The store uses `rusqlite` directly with a simple key-value schema (`order_id TEXT PRIMARY KEY, data TEXT`).

### PaymentProviders

The `PaymentProvider` trait defines the boundary between the daemon core and external payment APIs. Each provider implements `create_payment`, `check_status`, `cancel_payment`, and `poll_config`. V1 ships with two implementations: `SquareProvider` (fiat/card payments via the Square Checkout API) and `StrikeProvider` (Bitcoin/Lightning payments via the Strike Invoices API). Providers communicate with external APIs over HTTPS using `reqwest`. The trait design allows adding new providers (e.g., BTCPay Server) by implementing the trait without modifying core daemon code.

## Polling Lifecycle

The polling engine follows a four-phase lifecycle per payment:

1. **Idle** -- No pending payments exist. The engine loop runs but performs no API calls, consuming negligible resources.
2. **Active** -- A payment is registered via `register()`. The engine begins polling at the provider's `initial_interval` (e.g., 3 seconds for Strike lightning invoices, 5 seconds for Square checkouts). Each poll calls `check_status` on the provider.
3. **Backoff** -- If a poll returns `Pending` (no status change), the interval is multiplied by the provider's `backoff_multiplier` (e.g., 1.5x), up to the provider's `max_interval` ceiling. This reduces API load for slow-paying customers while still detecting completion promptly.
4. **Completion / Expiry** -- When `check_status` returns `Completed`, `Failed`, or `Expired` (or the payment's `expires_at` timestamp is reached), the engine emits a `PollingEvent`, removes the entry from its internal map, and returns to idle (or continues polling other active payments).

The transition back to idle is immediate -- the engine never polls a terminal payment. Provider-specific rate limit strategies (`HeaderMonitor`, `FixedBudget`) can further throttle polling to stay within API quotas.
