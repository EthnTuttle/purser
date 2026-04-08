//! Criteria #35: Full fiat checkout via Square sandbox.
//!
//! Flow: order → payment-request (with checkout_url) → payment → status-update (paid)
//!
//! Requires: SQUARE_SANDBOX_KEY, SQUARE_SANDBOX_LOCATION

use std::time::Duration;

use crate::common::{self, TestFixture};

/// Full fiat checkout flow against Square sandbox.
///
/// 1. Start daemon with Square sandbox config
/// 2. Send fiat order via MDK-encrypted message
/// 3. Receive payment-request with checkout_url
/// 4. Simulate payment completion via Square sandbox API
/// 5. Verify status-update with status: "paid" and valid payment_id
#[tokio::test]
#[ignore]
async fn test_full_fiat_checkout() {
    if !common::has_square_sandbox() {
        eprintln!("SKIP: SQUARE_SANDBOX_KEY or SQUARE_SANDBOX_LOCATION not set");
        return;
    }

    let square_key = std::env::var("SQUARE_SANDBOX_KEY").unwrap();
    let square_location = std::env::var("SQUARE_SANDBOX_LOCATION").unwrap();

    let fixture = TestFixture::setup(&common::square_provider_toml())
        .await
        .expect("fixture setup failed");

    // Build and send order
    let order_id = uuid::Uuid::new_v4().to_string();
    let order_json = serde_json::json!({
        "version": 1,
        "msg_type": "order",
        "order_id": order_id,
        "items": [{
            "product_id": "e2e-widget",
            "name": "E2E Test Widget",
            "quantity": 1,
            "variant": "standard",
            "unit_price_usd": "10.00"
        }],
        "order_type": "physical",
        "shipping": {
            "name": "E2E Test",
            "address_line_1": "123 Test St",
            "city": "Testville",
            "state": "TX",
            "zip": "78701",
            "country": "US"
        },
        "payment_method": "fiat",
        "currency": "USD"
    })
    .to_string();

    eprintln!("[test] sending order {order_id}...");
    let _group_id = fixture
        .customer
        .send_order(common::TEST_MERCHANT_PUBKEY_HEX, &order_json)
        .await
        .expect("failed to send order");

    // Wait for payment-request
    eprintln!("[test] waiting for payment-request...");
    let payment_request_json = fixture
        .customer
        .wait_for_message(Duration::from_secs(60))
        .await
        .expect("did not receive payment-request");

    let payment_request: serde_json::Value =
        serde_json::from_str(&payment_request_json).expect("invalid payment-request JSON");

    assert_eq!(
        payment_request["msg_type"].as_str(),
        Some("payment-request"),
        "expected payment-request, got: {payment_request}"
    );
    assert_eq!(
        payment_request["order_id"].as_str(),
        Some(order_id.as_str()),
        "order_id mismatch"
    );

    let payment_id = payment_request["payment_id"]
        .as_str()
        .expect("no payment_id in payment-request");
    let checkout_url = payment_request["payment_details"]["checkout_url"]
        .as_str()
        .expect("no checkout_url in payment-request");

    eprintln!("[test] received payment-request: payment_id={payment_id}, checkout_url={checkout_url}");

    // Complete payment via Square sandbox API
    eprintln!("[test] completing Square sandbox payment...");
    common::complete_square_payment(&square_key, payment_id, &square_location)
        .await
        .expect("failed to complete Square payment");

    // Wait for status-update (paid)
    eprintln!("[test] waiting for status-update...");
    let status_update_json = fixture
        .customer
        .wait_for_message(Duration::from_secs(120))
        .await
        .expect("did not receive status-update");

    let status_update: serde_json::Value =
        serde_json::from_str(&status_update_json).expect("invalid status-update JSON");

    assert_eq!(
        status_update["msg_type"].as_str(),
        Some("status-update"),
        "expected status-update, got: {status_update}"
    );
    assert_eq!(
        status_update["order_id"].as_str(),
        Some(order_id.as_str()),
        "order_id mismatch in status-update"
    );
    assert_eq!(
        status_update["status"].as_str(),
        Some("paid"),
        "expected status 'paid', got: {}",
        status_update["status"]
    );
    assert!(
        status_update["payment_id"].as_str().is_some(),
        "missing payment_id in status-update"
    );

    eprintln!("[test] E2E fiat checkout PASSED — order {order_id} paid successfully");
}
