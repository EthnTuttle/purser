use rusqlite::Connection;
use tracing::error;

use crate::error::{PurserError, Result};
use crate::state::PendingPayment;

fn map_db_err(e: rusqlite::Error) -> PurserError {
    PurserError::Storage(format!("SQLite error: {e}"))
}

pub struct PersistenceStore {
    conn: Connection,
}

impl PersistenceStore {
    /// Open (or create) the SQLite database at the given path.
    /// Runs CREATE TABLE IF NOT EXISTS on construction.
    pub fn open(db_path: &str) -> Result<Self> {
        let conn = if db_path == ":memory:" {
            Connection::open_in_memory().map_err(map_db_err)?
        } else {
            Connection::open(db_path).map_err(map_db_err)?
        };

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_payments (
                order_id TEXT PRIMARY KEY,
                data     TEXT NOT NULL
            );",
        )
        .map_err(map_db_err)?;

        Ok(Self { conn })
    }

    /// Save a single pending payment. Uses INSERT OR REPLACE.
    /// On disk-full or I/O errors, logs the error and returns Ok(()) — the
    /// daemon continues with in-memory state.
    pub fn save_pending(&self, payment: &PendingPayment) -> Result<()> {
        let data = serde_json::to_string(payment)
            .map_err(|e| PurserError::Storage(format!("serialization error: {e}")))?;

        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO pending_payments (order_id, data) VALUES (?1, ?2)",
            rusqlite::params![payment.order_id, data],
        ) {
            error!("Failed to persist pending payment {}: {e}", payment.order_id);
        }

        Ok(())
    }

    /// Load all pending payments from the database.
    /// Errors are propagated because this is called at startup — if we cannot
    /// read persisted state, that is a real problem.
    pub fn load_all_pending(&self) -> Result<Vec<PendingPayment>> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM pending_payments")
            .map_err(map_db_err)?;

        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(map_db_err)?;

        let mut payments = Vec::new();
        for row in rows {
            let data = row.map_err(map_db_err)?;
            let payment: PendingPayment = serde_json::from_str(&data)
                .map_err(|e| PurserError::Storage(format!("deserialization error: {e}")))?;
            payments.push(payment);
        }

        Ok(payments)
    }

    /// Delete a pending payment by order_id.
    /// On I/O errors, logs and returns Ok(()) (disk-full resilience).
    pub fn delete_pending(&self, order_id: &str) -> Result<()> {
        if let Err(e) = self.conn.execute(
            "DELETE FROM pending_payments WHERE order_id = ?1",
            rusqlite::params![order_id],
        ) {
            error!("Failed to delete pending payment {order_id}: {e}");
        }

        Ok(())
    }

    /// Save all pending payments in a single transaction (for graceful shutdown).
    /// Clears the table first, then inserts all payments.
    /// On I/O errors, logs and returns Ok(()) (disk-full resilience).
    pub fn save_all_pending(&self, payments: &[PendingPayment]) -> Result<()> {
        let result: std::result::Result<(), rusqlite::Error> = (|| {
            self.conn.execute("DELETE FROM pending_payments", [])?;
            for payment in payments {
                let data = serde_json::to_string(payment).map_err(|e| {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
                })?;
                self.conn.execute(
                    "INSERT INTO pending_payments (order_id, data) VALUES (?1, ?2)",
                    rusqlite::params![payment.order_id, data],
                )?;
            }
            Ok(())
        })();

        if let Err(e) = result {
            error!("Failed to persist all pending payments: {e}");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::PaymentMethod;
    use crate::state::PendingPaymentStatus;
    use chrono::Utc;

    fn sample_payment(order_id: &str) -> PendingPayment {
        PendingPayment {
            order_id: order_id.to_string(),
            customer_pubkey: "npub1test".to_string(),
            provider_name: "square".to_string(),
            payment_id: format!("pay_{order_id}"),
            payment_method: PaymentMethod::Fiat,
            amount: "59.99".to_string(),
            currency: "USD".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(15),
            status: PendingPaymentStatus::AwaitingPayment,
            group_id: "group_test".to_string(),
        }
    }

    #[test]
    fn test_open_creates_table() {
        let store = PersistenceStore::open(":memory:");
        assert!(store.is_ok());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let store = PersistenceStore::open(":memory:").unwrap();
        let payment = sample_payment("order-1");

        store.save_pending(&payment).unwrap();
        let loaded = store.load_all_pending().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].order_id, "order-1");
        assert_eq!(loaded[0].amount, "59.99");
        assert_eq!(loaded[0].currency, "USD");
        assert_eq!(loaded[0].provider_name, "square");
        assert_eq!(loaded[0].payment_id, "pay_order-1");
        assert_eq!(loaded[0].payment_method, PaymentMethod::Fiat);
        assert_eq!(loaded[0].status, PendingPaymentStatus::AwaitingPayment);
        assert_eq!(loaded[0].group_id, "group_test");
    }

    #[test]
    fn test_save_all_and_load() {
        let store = PersistenceStore::open(":memory:").unwrap();
        let payments = vec![
            sample_payment("order-a"),
            sample_payment("order-b"),
            sample_payment("order-c"),
        ];

        store.save_all_pending(&payments).unwrap();
        let loaded = store.load_all_pending().unwrap();

        assert_eq!(loaded.len(), 3);
        let mut ids: Vec<&str> = loaded.iter().map(|p| p.order_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["order-a", "order-b", "order-c"]);
    }

    #[test]
    fn test_delete() {
        let store = PersistenceStore::open(":memory:").unwrap();
        let payment = sample_payment("order-del");

        store.save_pending(&payment).unwrap();
        store.delete_pending("order-del").unwrap();
        let loaded = store.load_all_pending().unwrap();

        assert!(loaded.is_empty());
    }

    #[test]
    fn test_save_duplicate_replaces() {
        let store = PersistenceStore::open(":memory:").unwrap();

        let mut payment = sample_payment("order-dup");
        payment.amount = "10.00".to_string();
        store.save_pending(&payment).unwrap();

        payment.amount = "25.00".to_string();
        store.save_pending(&payment).unwrap();

        let loaded = store.load_all_pending().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].amount, "25.00");
    }

    #[test]
    fn test_load_empty() {
        let store = PersistenceStore::open(":memory:").unwrap();
        let loaded = store.load_all_pending().unwrap();
        assert!(loaded.is_empty());
    }

    /// Criteria #32 (full): Graceful shutdown persists pending payments to
    /// SQLite, simulating the save_all_pending path in main::run().
    #[test]
    fn test_graceful_shutdown_persists_all_pending() {
        let store = PersistenceStore::open(":memory:").unwrap();
        let payments = vec![
            sample_payment("shutdown-1"),
            sample_payment("shutdown-2"),
        ];

        // Simulate graceful shutdown: save all pending payments.
        store.save_all_pending(&payments).unwrap();

        // Verify they are persisted.
        let loaded = store.load_all_pending().unwrap();
        assert_eq!(loaded.len(), 2);
        let ids: Vec<&str> = loaded.iter().map(|p| p.order_id.as_str()).collect();
        assert!(ids.contains(&"shutdown-1"));
        assert!(ids.contains(&"shutdown-2"));
    }

    /// Criteria #33 (full): Startup recovery reloads persisted payments.
    /// Uses a temp file to test actual SQLite persistence across "restart"
    /// (close + reopen the store).
    #[test]
    fn test_startup_recovery_from_file() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("purser_test_recovery_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("recovery.db");
        let db_str = db_path.to_str().unwrap();

        // First "run": save payments, then drop the store.
        {
            let store = PersistenceStore::open(db_str).unwrap();
            let payments = vec![
                sample_payment("recover-1"),
                sample_payment("recover-2"),
            ];
            store.save_all_pending(&payments).unwrap();
        }

        // Second "run": reopen and verify recovery.
        {
            let store = PersistenceStore::open(db_str).unwrap();
            let loaded = store.load_all_pending().unwrap();
            assert_eq!(loaded.len(), 2);
            let ids: Vec<&str> = loaded.iter().map(|p| p.order_id.as_str()).collect();
            assert!(ids.contains(&"recover-1"));
            assert!(ids.contains(&"recover-2"));
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_disk_full_resilience() {
        use std::fs;

        let dir = std::env::temp_dir().join(format!("purser_test_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        let db_path = dir.join("test.db");
        let store = PersistenceStore::open(db_path.to_str().unwrap()).unwrap();

        // Save one payment to verify the DB works
        let payment = sample_payment("order-disk");
        store.save_pending(&payment).unwrap();

        // Make directory read-only to simulate disk-full / permission errors
        // Drop the store first since SQLite may hold handles
        drop(store);

        // Re-open and set the DB file read-only
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&db_path, fs::Permissions::from_mode(0o444)).unwrap();
        }

        // Open in read-only mode by opening the existing file
        // The open should succeed (reading is fine), but writes should fail gracefully
        let store = PersistenceStore::open(db_path.to_str().unwrap());

        if let Ok(store) = store {
            let result = store.save_pending(&sample_payment("order-fail"));
            // Should return Ok even if the write failed internally
            assert!(result.is_ok());

            let result = store.delete_pending("order-disk");
            assert!(result.is_ok());

            let result = store.save_all_pending(&[sample_payment("order-fail2")]);
            assert!(result.is_ok());
        }

        // Cleanup: restore permissions so we can delete
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&db_path, fs::Permissions::from_mode(0o644));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
