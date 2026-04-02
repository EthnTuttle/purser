//! Shared E2E test utilities: daemon lifecycle, sandbox config, skip logic.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
    pub fn spawn(sandbox_providers: &str) -> Result<Self, String> {
        let config_dir =
            std::env::temp_dir().join(format!("purser_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&config_dir).map_err(|e| format!("mkdir: {e}"))?;

        // Write config.toml
        let config_content = format!(
            r#"
merchant_npub = "npub1e2etestmerchant"
relays = ["wss://relay.damus.io"]

{sandbox_providers}

[storage]
db_path = "{db_path}"
"#,
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

        // Spawn the daemon
        let mut child = Command::new("cargo")
            .args(["run", "--release", "--"])
            .current_dir(&config_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;

        // Wait for "daemon running" in stderr (tracing output)
        let stderr = child.stderr.take().ok_or("no stderr")?;
        let reader = BufReader::new(stderr);
        let mut found = false;

        for line in reader.lines() {
            let line = line.map_err(|e| format!("read: {e}"))?;
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
