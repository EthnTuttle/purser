//! Criteria #37: Fiat checkout + shipping update.
//!
//! Flow: Complete fiat checkout (#35), then merchant sends shipping update
//! with status: "shipped" and tracking object.
//!
//! Requires: SQUARE_SANDBOX_KEY, SQUARE_SANDBOX_LOCATION

use crate::common;

/// Fiat checkout followed by shipping status update.
///
/// 1. Complete full fiat checkout (same as #35)
/// 2. Merchant sends shipping update via MDK
/// 3. Verify status-update with status: "shipped" includes tracking object
#[test]
#[ignore]
fn test_checkout_with_shipping_update() {
    if !common::has_square_sandbox() {
        eprintln!("SKIP: SQUARE_SANDBOX_KEY or SQUARE_SANDBOX_LOCATION not set");
        return;
    }

    // TODO: Once real MDK is wired into NostrClient, this test will:
    // 1. Complete the fiat checkout flow from #35
    // 2. Merchant sends a shipping status-update via MDK with:
    //    - status: "shipped"
    //    - tracking: { carrier: "USPS", tracking_number: "...", url: "..." }
    // 3. Verify the customer receives the status-update with tracking object

    eprintln!("E2E shipping update: sandbox configured");

    // Full E2E flow will be implemented when NostrClient uses real MDK.
}
