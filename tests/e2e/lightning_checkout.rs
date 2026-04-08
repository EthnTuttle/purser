//! Criteria #36: Full lightning checkout via Strike sandbox.
//!
//! Flow: order → payment-request (with lightning_invoice) → invoice payment → status-update (paid)
//!
//! Requires: STRIKE_SANDBOX_KEY

use std::time::Duration;

use crate::common::{self, TestFixture};

/// Full lightning checkout flow against Strike sandbox.
///
/// 1. Start daemon with Strike sandbox config
/// 2. Send lightning order via MDK-encrypted message
/// 3. Receive payment-request with lightning_invoice
/// 4. Simulate invoice payment via Strike sandbox API
/// 5. Verify status-update with status: "paid" and valid payment_id
#[tokio::test]
#[ignore]
async fn test_full_lightning_checkout() {
    if !common::has_strike_sandbox() {
        eprintln!("SKIP: STRIKE_SANDBOX_KEY not set");
        return;
    }

    let strike_key = std::env::var("STRIKE_SANDBOX_KEY").unwrap();

    let fixture = TestFixture::setup(common::strike_provider_toml())
        .await
        .expect("fixture setup failed");

    // Build and send lightning order
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
        "payment_method": "lightning",
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
    let lightning_invoice = payment_request["payment_details"]["lightning_invoice"]
        .as_str()
        .expect("no lightning_invoice in payment-request");

    eprintln!("[test] received payment-request: payment_id={payment_id}, invoice={}", &lightning_invoice[..40.min(lightning_invoice.len())]);

    // Pay the invoice via Strike sandbox
    eprintln!("[test] paying Strike sandbox invoice...");
    common::pay_strike_sandbox_invoice(&strike_key, payment_id)
        .await
        .expect("failed to pay Strike invoice");

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

    eprintln!("[test] E2E lightning checkout PASSED — order {order_id} paid successfully");
}
