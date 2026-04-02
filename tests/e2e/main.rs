//! End-to-end test harness for Purser — Issue #12
//!
//! Exercises the full checkout flow against a running daemon with sandbox APIs.
//! Tests are `#[ignore]` by default and only run when sandbox API keys are
//! configured via environment variables.
//!
//! Run with: `cargo test --test e2e -- --ignored`
//!
//! Required env vars for full E2E:
//! - SQUARE_SANDBOX_KEY — Square sandbox API key
//! - SQUARE_SANDBOX_LOCATION — Square sandbox location ID
//! - STRIKE_SANDBOX_KEY — Strike sandbox API key
//! - PURSER_E2E_RELAYS — comma-separated relay URLs (default: wss://relay.damus.io)
//!
//! Spec refs: §7 criteria #35, #36, #37

mod common;

mod fiat_checkout;
mod lightning_checkout;
mod shipping_update;
