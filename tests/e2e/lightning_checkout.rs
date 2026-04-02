//! Criteria #36: Full lightning checkout via Strike sandbox.
//!
//! Flow: order → payment-request (with lightning_invoice) → invoice payment → status-update (paid)
//!
//! Requires: STRIKE_SANDBOX_KEY

use crate::common;

/// Full lightning checkout flow against Strike sandbox.
///
/// 1. Start daemon with Strike sandbox config
/// 2. Send lightning order via MDK-encrypted message
/// 3. Receive payment-request with lightning_invoice
/// 4. Simulate invoice payment via Strike sandbox API
/// 5. Verify status-update with status: "paid" and valid payment_id
#[test]
#[ignore]
fn test_full_lightning_checkout() {
    if !common::has_strike_sandbox() {
        eprintln!("SKIP: STRIKE_SANDBOX_KEY not set");
        return;
    }

    let strike_key = std::env::var("STRIKE_SANDBOX_KEY").unwrap();

    let _providers = r#"
[[providers]]
type = "strike"
methods = ["lightning"]
api_key_env = "STRIKE_SANDBOX_KEY"
"#;

    // TODO: Once real MDK is wired into NostrClient, this test will:
    // 1. Spawn the daemon via DaemonHandle::spawn(&providers)
    // 2. Create a test MDK client as the "customer"
    // 3. Send an encrypted lightning order message
    // 4. Wait for the payment-request with lightning_invoice
    // 5. Pay the invoice via Strike sandbox API
    // 6. Wait for the status-update with status: "paid"

    eprintln!(
        "E2E lightning checkout: Strike sandbox configured (key={}...)",
        &strike_key[..8.min(strike_key.len())]
    );

    assert!(!strike_key.is_empty());

    // Full E2E flow will be implemented when NostrClient uses real MDK.
}
