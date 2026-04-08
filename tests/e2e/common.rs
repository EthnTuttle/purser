//! Shared E2E test utilities: daemon lifecycle, customer MDK client, sandbox helpers.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mdk_core::prelude::*;
use mdk_memory_storage::MdkMemoryStorage;
use nostr::event::builder::EventBuilder;
use nostr::{Keys, Kind, PublicKey, RelayUrl};
use nostr_sdk::Client;
use tokio::task::JoinHandle;

/// Test merchant nsec (matches src/test_keys.rs).
pub const TEST_MERCHANT_NSEC: &str =
    "nsec1x3rxtm6waw62n2meqvx0zuyya9z9t9ru0sxlxva2ah2lt57w2agqz9773y";

/// Test merchant npub.
pub const TEST_MERCHANT_NPUB: &str =
    "npub1x2zp9zxzawpdnm6shtje3jqu4nv00906u8qec0w2h2s0r7dz5q6sf6juy0";

/// Test merchant hex pubkey.
pub const TEST_MERCHANT_PUBKEY_HEX: &str =
    "32841288c2eb82d9ef50bae598c81cacd8f795fae1c19c3dcabaa0f1f9a2a035";

/// Default relays for E2E tests.
pub fn e2e_relays() -> Vec<String> {
    std::env::var("PURSER_E2E_RELAYS")
        .unwrap_or_else(|_| "wss://relay.damus.io".to_string())
        .split(',')
        .map(|s| s.trim().to_string())
        .collect()
}

/// Check if Square sandbox credentials are available.
pub fn has_square_sandbox() -> bool {
    std::env::var("SQUARE_SANDBOX_KEY").is_ok()
        && std::env::var("SQUARE_SANDBOX_LOCATION").is_ok()
}

/// Check if Strike sandbox credentials are available.
pub fn has_strike_sandbox() -> bool {
    std::env::var("STRIKE_SANDBOX_KEY").is_ok()
}

/// Handle to a running Purser daemon process.
/// Sends SIGTERM on drop for clean shutdown.
pub struct DaemonHandle {
    child: Option<Child>,
    config_dir: PathBuf,
}

impl DaemonHandle {
    /// Spawn a Purser daemon with a temporary config pointing to sandbox APIs.
    ///
    /// Writes config.toml and products.toml to a temp directory, then spawns
    /// `cargo run` as a child process. Waits for the "daemon running" log line
    /// before returning.
    pub fn spawn(sandbox_providers: &str, merchant_nsec: &str) -> Result<Self, String> {
        let config_dir =
            std::env::temp_dir().join(format!("purser_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("mkdir: {e}"))?;

        let relays = e2e_relays();
        let relays_toml: Vec<String> = relays.iter().map(|r| format!("\"{r}\"")).collect();

        // Write config.toml
        let config_content = format!(
            r#"
merchant_npub = "{merchant_npub}"
relays = [{relays}]

{sandbox_providers}

[storage]
db_path = "{db_path}"
"#,
            merchant_npub = TEST_MERCHANT_NPUB,
            relays = relays_toml.join(", "),
            db_path = config_dir.join("e2e.db").display()
        );
        std::fs::write(config_dir.join("config.toml"), config_content)
            .map_err(|e| format!("write config: {e}"))?;

        // Write products.toml
        let products_content = r#"
[[products]]
id = "e2e-widget"
name = "E2E Test Widget"
type = "physical"
price_usd = "10.00"
variants = ["standard"]
active = true
"#;
        std::fs::write(config_dir.join("products.toml"), products_content)
            .map_err(|e| format!("write products: {e}"))?;

        // Build env vars to pass through
        let mut cmd = Command::new("cargo");
        cmd.args(["run", "--release", "--"])
            .current_dir(&config_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("MERCHANT_NSEC", merchant_nsec);

        // Pass through sandbox API keys if set
        if let Ok(v) = std::env::var("SQUARE_SANDBOX_KEY") {
            cmd.env("SQUARE_SANDBOX_KEY", v);
        }
        if let Ok(v) = std::env::var("SQUARE_SANDBOX_LOCATION") {
            cmd.env("SQUARE_SANDBOX_LOCATION", v);
        }
        if let Ok(v) = std::env::var("STRIKE_SANDBOX_KEY") {
            cmd.env("STRIKE_SANDBOX_KEY", v);
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;

        // Wait for "daemon running" in stderr (tracing output) with 60s timeout
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let reader = BufReader::new(stderr);
        let mut found = false;
        let startup_deadline = std::time::Instant::now() + Duration::from_secs(60);

        for line in reader.lines() {
            if std::time::Instant::now() > startup_deadline {
                let _ = child.kill();
                return Err("daemon startup timed out (60s)".into());
            }
            let line = line.map_err(|e| format!("read: {e}"))?;
            eprintln!("[daemon] {line}");
            if line.contains("purser daemon running") {
                found = true;
                break;
            }
        }

        if !found {
            let _ = child.kill();
            return Err("daemon did not emit 'daemon running' log line".into());
        }

        Ok(Self {
            child: Some(child),
            config_dir,
        })
    }

    /// Get the config directory path (for constructing test orders).
    #[allow(dead_code)]
    pub fn config_dir(&self) -> &PathBuf {
        &self.config_dir
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Send SIGTERM on Unix via kill command
            #[cfg(unix)]
            {
                let pid = child.id();
                let _ = Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();

                // Wait up to 10 seconds for clean shutdown
                let deadline = std::time::Instant::now() + Duration::from_secs(10);
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        _ => {
                            let _ = child.kill();
                            break;
                        }
                    }
                }
            }

            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
        }

        // Clean up temp directory
        let _ = std::fs::remove_dir_all(&self.config_dir);
    }
}

// ---------------------------------------------------------------------------
// CustomerClient — lightweight MDK client simulating a customer
// ---------------------------------------------------------------------------

/// A customer-side MDK client for E2E tests.
///
/// Creates its own Nostr keypair, connects to relays, publishes key packages,
/// and subscribes to incoming messages. Messages arrive via a background relay
/// subscription and are delivered through an internal channel, eliminating
/// poll-sleep loops.
pub struct CustomerClient {
    mdk: Arc<MDK<MdkMemoryStorage>>,
    keys: Keys,
    nostr_client: Client,
    relay_urls: Vec<RelayUrl>,
    /// Maps hex nostr_group_id → MLS GroupId
    groups: Mutex<HashMap<String, GroupId>>,
    /// Receives decrypted application messages from the background subscription task.
    message_rx: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<String>>,
    /// Background subscription task handle; aborted on drop.
    _subscription_handle: JoinHandle<()>,
}

impl CustomerClient {
    /// Create a new customer client with a random keypair, connected to the given relays.
    ///
    /// Establishes a relay subscription for incoming messages so that
    /// `wait_for_message` can return immediately when a message arrives.
    pub async fn new(relays: &[String]) -> Result<Self, String> {
        let keys = Keys::generate();
        let mdk = Arc::new(MDK::new(MdkMemoryStorage::default()));

        let relay_urls: Vec<RelayUrl> = relays
            .iter()
            .filter_map(|r| RelayUrl::parse(r).ok())
            .collect();

        if relay_urls.is_empty() {
            return Err("no valid relay URLs".into());
        }

        let nostr_client = Client::new(keys.clone());
        for url in &relay_urls {
            nostr_client
                .add_relay(url.clone())
                .await
                .map_err(|e| format!("add relay: {e}"))?;
        }
        nostr_client.connect().await;

        // Publish a key package so the merchant can create a group with us
        let (kp_encoded, tags, _) = mdk
            .create_key_package_for_event(&keys.public_key(), relay_urls.clone())
            .map_err(|e| format!("create key package: {e}"))?;

        let kp_event = EventBuilder::new(Kind::MlsKeyPackage, kp_encoded)
            .tags(tags)
            .sign(&keys)
            .await
            .map_err(|e| format!("sign key package: {e}"))?;

        nostr_client
            .send_event(&kp_event)
            .await
            .map_err(|e| format!("publish key package: {e}"))?;

        // Small delay for relay propagation
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Subscribe to incoming messages (GiftWrap welcomes + MLS group messages)
        let filter = nostr::Filter::new()
            .pubkey(keys.public_key())
            .kinds(vec![Kind::MlsGroupMessage, Kind::GiftWrap]);

        nostr_client
            .subscribe(filter, None)
            .await
            .map_err(|e| format!("subscribe: {e}"))?;

        let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
        let sub_mdk = Arc::clone(&mdk);
        let sub_keys = keys.clone();
        let sub_client = nostr_client.clone();

        let subscription_handle = tokio::spawn(async move {
            let _ = sub_client
                .handle_notifications(|notification| {
                    let tx = tx.clone();
                    let mdk = Arc::clone(&sub_mdk);
                    let keys = sub_keys.clone();
                    async move {
                        if let nostr_sdk::RelayPoolNotification::Event { event, .. } = notification
                        {
                            match event.kind {
                                Kind::GiftWrap => {
                                    if let Ok(unwrapped) =
                                        nostr::nips::nip59::extract_rumor(&keys, &event).await
                                    {
                                        let _ =
                                            mdk.process_welcome(&event.id, &unwrapped.rumor);
                                        let welcomes =
                                            mdk.get_pending_welcomes(None).unwrap_or_default();
                                        for welcome in &welcomes {
                                            if let Ok(()) = mdk.accept_welcome(welcome) {
                                                eprintln!(
                                                    "[customer] accepted welcome for group: {}",
                                                    welcome.group_name
                                                );
                                            }
                                        }
                                    }
                                }
                                Kind::MlsGroupMessage => {
                                    if let Ok(MessageProcessingResult::ApplicationMessage(msg)) =
                                        mdk.process_message(&event)
                                    {
                                        eprintln!(
                                            "[customer] received message: {}",
                                            &msg.content[..80.min(msg.content.len())]
                                        );
                                        if tx.send(msg.content).await.is_err() {
                                            return Ok(true); // channel closed, stop
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(false)
                    }
                })
                .await;
        });

        Ok(Self {
            mdk,
            keys,
            nostr_client,
            relay_urls,
            groups: Mutex::new(HashMap::new()),
            message_rx: tokio::sync::Mutex::new(rx),
            _subscription_handle: subscription_handle,
        })
    }

    /// Get the customer's public key as hex string.
    pub fn pubkey_hex(&self) -> String {
        self.keys.public_key().to_string()
    }

    /// Send an order message to the merchant by creating an MLS group.
    ///
    /// Creates a new MLS group with the merchant, sends the order as an
    /// encrypted message, and returns the nostr_group_id.
    pub async fn send_order(
        &self,
        merchant_pubkey: &str,
        order_json: &str,
    ) -> Result<String, String> {
        let merchant_pk = PublicKey::parse(merchant_pubkey)
            .map_err(|e| format!("parse merchant pubkey: {e}"))?;

        // Fetch merchant's key package from relays
        let filter = nostr::Filter::new()
            .author(merchant_pk)
            .kind(Kind::MlsKeyPackage)
            .limit(1);

        let events = self
            .nostr_client
            .fetch_events(filter, Duration::from_secs(15))
            .await
            .map_err(|e| format!("fetch merchant key package: {e}"))?;

        let kp_event = events
            .into_iter()
            .next()
            .ok_or_else(|| "no key package found for merchant".to_string())?;

        // Create MLS group with merchant
        let config = NostrGroupConfigData::new(
            format!("order-{}", uuid::Uuid::new_v4()),
            "E2E test checkout".to_owned(),
            None,
            None,
            None,
            self.relay_urls.clone(),
            vec![self.keys.public_key(), merchant_pk],
        );

        let result = self
            .mdk
            .create_group(&self.keys.public_key(), vec![kp_event], config)
            .map_err(|e| format!("create group: {e}"))?;

        let group = result.group;
        let mls_group_id = GroupId::from_slice(group.mls_group_id.as_slice());
        let nostr_group_id = hex::encode(&group.nostr_group_id);

        self.mdk
            .merge_pending_commit(&mls_group_id)
            .map_err(|e| format!("merge pending commit: {e}"))?;

        // Gift-wrap and publish welcome messages to merchant
        for welcome_rumor in &result.welcome_rumors {
            let gift_wrapped = EventBuilder::gift_wrap(
                &self.keys,
                &merchant_pk,
                welcome_rumor.clone(),
                [],
            )
            .await
            .map_err(|e| format!("gift wrap welcome: {e}"))?;

            self.nostr_client
                .send_event(&gift_wrapped)
                .await
                .map_err(|e| format!("publish welcome: {e}"))?;
        }

        self.groups
            .lock()
            .unwrap()
            .insert(nostr_group_id.clone(), mls_group_id);

        // Send the order message inside the group
        let rumor =
            EventBuilder::new(Kind::Custom(9), order_json).build(self.keys.public_key());

        let message_event = self
            .mdk
            .create_message(
                self.groups.lock().unwrap().get(&nostr_group_id).unwrap(),
                rumor,
            )
            .map_err(|e| format!("create message: {e}"))?;

        self.nostr_client
            .send_event(&message_event)
            .await
            .map_err(|e| format!("publish order message: {e}"))?;

        eprintln!("[customer] sent order in group {nostr_group_id}");
        Ok(nostr_group_id)
    }

    /// Wait for a decrypted message from the merchant.
    ///
    /// Blocks until the background subscription delivers an application message
    /// or the timeout expires. No polling — messages arrive via relay subscription.
    pub async fn wait_for_message(&self, timeout: Duration) -> Result<String, String> {
        let mut rx = self.message_rx.lock().await;
        tokio::time::timeout(timeout, rx.recv())
            .await
            .map_err(|_| "timeout waiting for message".to_string())?
            .ok_or_else(|| "message channel closed".to_string())
    }
}

impl Drop for CustomerClient {
    fn drop(&mut self) {
        self._subscription_handle.abort();
    }
}

// ---------------------------------------------------------------------------
// TestFixture — reusable harness for daemon + customer lifecycle
// ---------------------------------------------------------------------------

/// Bundles a running daemon and customer client for E2E test scenarios.
///
/// Handles all lifecycle: daemon spawn, customer key generation, relay
/// subscription setup, and cleanup on drop. Test scenarios only need to
/// focus on the payment flow.
pub struct TestFixture {
    /// Kept alive for its `Drop` impl (SIGTERM + cleanup).
    #[allow(dead_code)]
    pub daemon: DaemonHandle,
    pub customer: CustomerClient,
}

impl TestFixture {
    /// Spawn a daemon with the given provider config and create a customer client.
    pub async fn setup(provider_config: &str) -> Result<Self, String> {
        let relays = e2e_relays();
        eprintln!("[test] spawning daemon...");
        let daemon = DaemonHandle::spawn(provider_config, TEST_MERCHANT_NSEC)
            .map_err(|e| format!("daemon spawn: {e}"))?;
        eprintln!("[test] daemon running");

        eprintln!("[test] creating customer client...");
        let customer = CustomerClient::new(&relays)
            .await
            .map_err(|e| format!("customer setup: {e}"))?;
        eprintln!("[test] customer pubkey: {}", customer.pubkey_hex());

        Ok(Self { daemon, customer })
    }
}

/// Strike sandbox provider TOML fragment.
pub fn strike_provider_toml() -> &'static str {
    r#"
[[providers]]
type = "strike"
methods = ["lightning"]
api_key_env = "STRIKE_SANDBOX_KEY"
"#
}

/// Square sandbox provider TOML fragment.
pub fn square_provider_toml() -> String {
    r#"
[[providers]]
type = "square"
methods = ["fiat"]
api_key_env = "SQUARE_SANDBOX_KEY"
location_id_env = "SQUARE_SANDBOX_LOCATION"
base_url = "https://connect.squareupsandbox.com/v2"
"#
    .to_string()
}

// ---------------------------------------------------------------------------
// Square sandbox helpers
// ---------------------------------------------------------------------------

/// Complete a Square sandbox payment by paying the order via the Payments API.
///
/// 1. Fetch the payment link to get the associated order ID
/// 2. Create a payment with the sandbox test nonce `cnon:card-nonce-ok`
pub async fn complete_square_payment(
    api_key: &str,
    payment_link_id: &str,
    location_id: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let base_url = "https://connect.squareupsandbox.com/v2";

    // Step 1: Get the payment link to find the order ID
    let link_resp = client
        .get(format!(
            "{base_url}/online-checkout/payment-links/{payment_link_id}"
        ))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| format!("get payment link: {e}"))?;

    let link_json: serde_json::Value = link_resp
        .json()
        .await
        .map_err(|e| format!("parse payment link response: {e}"))?;

    eprintln!("[square] payment link response: {link_json}");

    let order_id = link_json["payment_link"]["order_id"]
        .as_str()
        .ok_or_else(|| "no order_id in payment link response".to_string())?;

    // Step 2: Get the order to find the total amount
    let order_resp = client
        .get(format!("{base_url}/orders/{order_id}"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send()
        .await
        .map_err(|e| format!("get order: {e}"))?;

    let order_json: serde_json::Value = order_resp
        .json()
        .await
        .map_err(|e| format!("parse order response: {e}"))?;

    eprintln!("[square] order response: {order_json}");

    let total_money = &order_json["order"]["total_money"];
    let amount = total_money["amount"]
        .as_i64()
        .ok_or_else(|| "no amount in order".to_string())?;
    let currency = total_money["currency"]
        .as_str()
        .unwrap_or("USD");

    // Step 3: Create a payment using the sandbox test nonce
    let payment_body = serde_json::json!({
        "source_id": "cnon:card-nonce-ok",
        "idempotency_key": format!("e2e-pay-{}", uuid::Uuid::new_v4()),
        "amount_money": {
            "amount": amount,
            "currency": currency,
        },
        "order_id": order_id,
        "location_id": location_id,
    });

    let pay_resp = client
        .post(format!("{base_url}/payments"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .json(&payment_body)
        .send()
        .await
        .map_err(|e| format!("create payment: {e}"))?;

    let pay_json: serde_json::Value = pay_resp
        .json()
        .await
        .map_err(|e| format!("parse payment response: {e}"))?;

    eprintln!("[square] payment response: {pay_json}");

    let status = pay_json["payment"]["status"].as_str().unwrap_or("unknown");
    if status == "COMPLETED" || status == "APPROVED" {
        eprintln!("[square] payment completed successfully");
        Ok(())
    } else if pay_json["errors"].is_array() {
        Err(format!("Square payment failed: {}", pay_json["errors"]))
    } else {
        // Payment may be pending — that's ok, polling will pick it up
        eprintln!("[square] payment status: {status}");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Strike sandbox helpers
// ---------------------------------------------------------------------------

/// Simulate paying a Strike sandbox invoice.
///
/// Attempts to use the sandbox pay endpoint, then polls until paid or timeout.
#[allow(dead_code)]
pub async fn pay_strike_sandbox_invoice(
    api_key: &str,
    invoice_id: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let base_url = "https://api.strike.me/v1";

    // Try the sandbox pay endpoint
    let _pay_resp = client
        .post(format!("{base_url}/invoices/{invoice_id}/pay"))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send()
        .await;

    // Poll until paid or timeout
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("timeout waiting for Strike invoice payment".into());
        }

        let resp = client
            .get(format!("{base_url}/invoices/{invoice_id}"))
            .header("Authorization", format!("Bearer {api_key}"))
            .send()
            .await
            .map_err(|e| format!("get invoice: {e}"))?;

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("parse invoice response: {e}"))?;

        let state = json["state"].as_str().unwrap_or("UNKNOWN");
        eprintln!("[strike] invoice state: {state}");

        if state == "PAID" {
            return Ok(());
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
