//! Criteria #35: Full fiat checkout via Square sandbox.
//!
//! Flow: order → payment-request (with checkout_url) → payment → status-update (paid)
//!
//! Requires: SQUARE_SANDBOX_KEY, SQUARE_SANDBOX_LOCATION

use crate::common;

/// Full fiat checkout flow against Square sandbox.
///
/// 1. Start daemon with Square sandbox config
/// 2. Send fiat order via MDK-encrypted message
/// 3. Receive payment-request with checkout_url
/// 4. Simulate payment completion via Square sandbox API
/// 5. Verify status-update with status: "paid" and valid payment_id
#[test]
#[ignore]
fn test_full_fiat_checkout() {
    if !common::has_square_sandbox() {
        eprintln!("SKIP: SQUARE_SANDBOX_KEY or SQUARE_SANDBOX_LOCATION not set");
        return;
    }

    let square_key = std::env::var("SQUARE_SANDBOX_KEY").unwrap();
    let square_location = std::env::var("SQUARE_SANDBOX_LOCATION").unwrap();

    let providers = format!(
        r#"
[[providers]]
type = "square"
methods = ["fiat"]
api_key_env = "SQUARE_SANDBOX_KEY"
location_id_env = "SQUARE_SANDBOX_LOCATION"
"#
    );

    // TODO: Once real MDK is wired into NostrClient, this test will:
    // 1. Spawn the daemon via DaemonHandle::spawn(&providers)
    // 2. Create a test MDK client as the "customer"
    // 3. Send an encrypted order message
    // 4. Wait for the payment-request response
    // 5. Hit the Square sandbox checkout URL to complete payment
    // 6. Wait for the status-update with status: "paid"

    eprintln!(
        "E2E fiat checkout: Square sandbox configured (key={}..., location={})",
        &square_key[..8.min(square_key.len())],
        &square_location
    );

    // Placeholder: verify credentials are accessible
    assert!(!square_key.is_empty());
    assert!(!square_location.is_empty());

    // Full E2E flow will be implemented when NostrClient uses real MDK.
    // For now, this test validates the test harness infrastructure and
    // sandbox credential availability.
}
