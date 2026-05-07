use std::path::Path;
use std::time::Duration;
use rusqlite::Connection;
use crate::common::{self, TestFixture};

fn verify_pending_payments_in_db(db_path: &Path, expected_count: usize) -> Result<Vec<String>, String> {
    let conn = Connection::open(db_path)
        .map_err(|e| format!("open db: {e}"))?;

    let mut stmt = conn
        .prepare("SELECT order_id FROM pending_payments")
        .map_err(|e| format!("prepare: {e}"))?;

    let ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| format!("query: {e}"))?
        .filter_map(|r| r.ok())
        .collect();

    if ids.len() != expected_count {
        return Err(format!("expected {} pending payments, got {}", expected_count, ids.len()));
    }

    Ok(ids)
}

#[tokio::test]
#[ignore]
async fn test_graceful_shutdown_with_pending_payments() {
    if !common::has_square_sandbox() {
        eprintln!("[test] skipping: SQUARE_SANDBOX_KEY or SQUARE_SANDBOX_LOCATION not set");
        return;
    }

    let square_key = std::env::var("SQUARE_SANDBOX_KEY").unwrap();
    let square_location = std::env::var("SQUARE_SANDBOX_LOCATION").unwrap();
    let _ = (square_key, square_location);

    let mut fixture = TestFixture::setup(&common::square_provider_toml())
        .await
        .expect("failed to setup test fixture");

    let config_dir = fixture.daemon.config_dir().clone();

    let order_id_1 = uuid::Uuid::new_v4().to_string();
    let order_json_1 = serde_json::json!({
        "version": 1,
        "type": "order",
        "order_id": order_id_1,
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
    });

    let _nostr_group_id_1 = fixture
        .customer
        .send_order(common::TEST_MERCHANT_PUBKEY_HEX, &order_json_1.to_string())
        .await
        .expect("failed to send order 1");

    let msg_1 = fixture
        .customer
        .wait_for_message(Duration::from_secs(60))
        .await
        .expect("timeout waiting for payment-request 1");

    let payment_msg_1: serde_json::Value = serde_json::from_str(&msg_1)
        .expect("failed to parse payment-request 1");

    assert_eq!(
        payment_msg_1["type"].as_str(),
        Some("payment-request"),
        "expected payment-request message"
    );

    let payment_id_1 = payment_msg_1["payment_id"]
        .as_str()
        .expect("missing payment_id in payment-request 1");
    eprintln!("[test] received payment-request 1: payment_id={}", payment_id_1);

    let order_id_2 = uuid::Uuid::new_v4().to_string();
    let order_json_2 = serde_json::json!({
        "version": 1,
        "type": "order",
        "order_id": order_id_2,
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
    });

    let _nostr_group_id_2 = fixture
        .customer
        .send_order(common::TEST_MERCHANT_PUBKEY_HEX, &order_json_2.to_string())
        .await
        .expect("failed to send order 2");

    let msg_2 = fixture
        .customer
        .wait_for_message(Duration::from_secs(60))
        .await
        .expect("timeout waiting for payment-request 2");

    let payment_msg_2: serde_json::Value = serde_json::from_str(&msg_2)
        .expect("failed to parse payment-request 2");

    assert_eq!(
        payment_msg_2["type"].as_str(),
        Some("payment-request"),
        "expected payment-request message"
    );

    let payment_id_2 = payment_msg_2["payment_id"]
        .as_str()
        .expect("missing payment_id in payment-request 2");
    eprintln!("[test] received payment-request 2: payment_id={}", payment_id_2);

    fixture.daemon.send_sigterm();

    let exited = fixture.daemon.wait_for_exit(Duration::from_secs(15));
    if !exited {
        panic!("daemon did not exit within 15 seconds after SIGTERM");
    }

    drop(fixture);

    let db_path = config_dir.join("e2e.db");
    eprintln!("[test] checking SQLite database at: {}", db_path.display());

    let pending_ids = verify_pending_payments_in_db(&db_path, 2)
        .expect("failed to verify pending payments in database");

    assert!(
        pending_ids.contains(&order_id_1),
        "order_id_1 not found in pending payments"
    );
    assert!(
        pending_ids.contains(&order_id_2),
        "order_id_2 not found in pending payments"
    );

    eprintln!("[test] verified {} pending payments persisted", pending_ids.len());

    std::fs::remove_dir_all(&config_dir).ok();
}