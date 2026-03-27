use thiserror::Error;

#[derive(Debug, Error)]
pub enum PurserError {
    // Config & startup errors
    #[error("configuration error: {0}")]
    Config(String),

    #[error("catalog error: {0}")]
    Catalog(String),

    // Order validation errors
    #[error("schema validation failed: {0}")]
    SchemaValidation(String),

    #[error("catalog validation failed: {0}")]
    CatalogValidation(String),

    #[error("duplicate order: {order_id}")]
    DuplicateOrder { order_id: String },

    #[error("unsupported payment method: {0}")]
    UnsupportedPaymentMethod(String),

    // Provider errors
    #[error("provider error ({provider}): {message}")]
    Provider { provider: String, message: String },

    #[error("payment not found: {0}")]
    PaymentNotFound(String),

    // Rate limiting
    #[error("rate limited: {0}")]
    RateLimited(String),

    #[error("concurrent session exists: {0}")]
    ConcurrentSession(String),

    // Nostr / MDK errors
    #[error("nostr error: {0}")]
    Nostr(String),

    #[error("MDK error: {0}")]
    Mdk(String),

    // Storage
    #[error("storage error: {0}")]
    Storage(String),

    // General
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, PurserError>;
