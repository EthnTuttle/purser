mod catalog;
mod config;
mod error;
mod messages;
mod nostr;
mod persistence;
mod pipeline;
mod polling;
mod providers;
mod ratelimit;
mod state;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{mpsc, RwLock};

use crate::error::PurserError;
use crate::persistence::PersistenceStore;
use crate::pipeline::{IncomingOrder, PipelineContext};
use crate::providers::square::SquareProvider;
use crate::providers::strike::StrikeProvider;
use crate::providers::PaymentProvider;
use crate::state::AppState;

#[tokio::main]
async fn main() {
    // 1. Initialize tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("purser=info".parse().unwrap()),
        )
        .init();

    if let Err(e) = run().await {
        tracing::error!("fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    // 2. Load config.
    let config = config::load_config("config.toml")?;
    tracing::info!(relays = config.relays.len(), "config loaded");

    // 3. Load catalog.
    let catalog = catalog::load_catalog("products.toml")?;
    tracing::info!(products = catalog.len(), "catalog loaded");

    // 4. Initialize providers from config.
    let providers = init_providers(&config)?;
    tracing::info!(count = providers.len(), "providers initialized");

    // 5. Initialize NostrClient.
    let nostr_client = Arc::new(
        nostr::NostrClient::new(&config.relays, &config.mdk.storage_type).await?,
    );

    // 6. Initialize PollingEngine.
    let (polling_engine, polling_rx) = polling::PollingEngine::new(
        providers.clone(),
        config.polling.partial_payment_margin_percent,
    );
    let polling_engine = Arc::new(polling_engine);

    // 7. Initialize RateLimiter.
    let rate_limiter = Arc::new(ratelimit::RateLimiter::new(config.rate_limits.clone()));

    // 8. Build AppState.
    let app_state = Arc::new(AppState {
        config: config.clone(),
        catalog,
        pending_payments: RwLock::new(HashMap::new()),
        providers,
    });

    // 9. Open persistence store.
    let persistence = PersistenceStore::open(&config.storage.db_path)?;
    tracing::info!(db_path = %config.storage.db_path, "persistence store opened");

    // 10. Startup recovery.
    recover_pending(&persistence, &app_state, &polling_engine).await?;

    // 11. Build PipelineContext.
    let ctx = Arc::new(PipelineContext {
        state: Arc::clone(&app_state),
        nostr: Arc::clone(&nostr_client),
        rate_limiter: Arc::clone(&rate_limiter),
        polling_engine: Arc::clone(&polling_engine),
    });

    // 12. Create order ingress channel.
    let (order_tx, order_rx) = mpsc::channel::<IncomingOrder>(256);

    // 13. Create shutdown signal channel.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // 14. Spawn tasks.

    // Task 1: Polling engine loop.
    let polling_handle = {
        let engine = Arc::clone(&polling_engine);
        tokio::spawn(async move {
            if let Err(e) = engine.run().await {
                tracing::error!("polling engine error: {e}");
            }
        })
    };

    // Task 2: Message listener (reads from order_rx, calls process_order).
    let listener_handle = {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            message_listener(order_rx, ctx).await;
        })
    };

    // Task 3: Event handler (reads from polling_rx, calls handle_polling_event).
    let event_handle = {
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            event_handler(polling_rx, ctx).await;
        })
    };

    // 15. Wait for SIGTERM/SIGINT.
    tracing::info!("purser daemon running — waiting for shutdown signal");
    let _ = order_tx; // Keep sender alive; drop below for shutdown.

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received SIGINT, shutting down");
        }
        _ = async {
            #[cfg(unix)]
            {
                let mut sigterm = tokio::signal::unix::signal(
                    tokio::signal::unix::SignalKind::terminate(),
                ).expect("failed to register SIGTERM handler");
                sigterm.recv().await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        } => {
            tracing::info!("received SIGTERM, shutting down");
        }
        _ = shutdown_rx.changed() => {}
    }

    // 16. Graceful shutdown.
    let _ = shutdown_tx.send(true);

    // Abort tasks with a 10-second deadline.
    let shutdown_deadline = tokio::time::sleep(std::time::Duration::from_secs(10));
    tokio::select! {
        _ = shutdown_deadline => {
            tracing::warn!("shutdown deadline reached, forcing abort");
        }
        _ = async {
            polling_handle.abort();
            listener_handle.abort();
            event_handle.abort();
            let _ = polling_handle.await;
            let _ = listener_handle.await;
            let _ = event_handle.await;
        } => {}
    }

    // Persist pending payments to SQLite.
    let pending = app_state.pending_payments.read().await;
    let payments: Vec<_> = pending.values().cloned().collect();
    drop(pending);

    if !payments.is_empty() {
        tracing::info!(count = payments.len(), "persisting pending payments");
        let _ = persistence.save_all_pending(&payments);
    }

    tracing::info!("purser daemon shut down cleanly");
    Ok(())
}

/// Initialize payment providers from config.
fn init_providers(
    config: &config::Config,
) -> Result<Vec<Arc<dyn PaymentProvider>>, Box<dyn std::error::Error>> {
    let mut providers: Vec<Arc<dyn PaymentProvider>> = Vec::new();

    for pc in &config.providers {
        let api_key = std::env::var(&pc.api_key_env).map_err(|_| {
            PurserError::Config(format!(
                "environment variable '{}' not set for provider '{}'",
                pc.api_key_env, pc.provider_type
            ))
        })?;

        match pc.provider_type.as_str() {
            "square" => {
                let location_id = if let Some(ref env_name) = pc.location_id_env {
                    std::env::var(env_name).map_err(|_| {
                        PurserError::Config(format!(
                            "environment variable '{env_name}' not set for Square location_id"
                        ))
                    })?
                } else {
                    return Err(Box::new(PurserError::Config(
                        "Square provider requires 'location_id_env' in config".to_string(),
                    )));
                };
                providers.push(Arc::new(SquareProvider::new(
                    api_key,
                    location_id,
                    None,
                    None,
                )));
            }
            "strike" => {
                providers.push(Arc::new(StrikeProvider::new(api_key)));
            }
            other => {
                return Err(Box::new(PurserError::Config(format!(
                    "unknown provider type: '{other}'"
                ))));
            }
        }
    }

    Ok(providers)
}

/// Recover pending payments from SQLite on startup.
async fn recover_pending(
    persistence: &PersistenceStore,
    state: &AppState,
    polling_engine: &polling::PollingEngine,
) -> Result<(), Box<dyn std::error::Error>> {
    let saved = persistence.load_all_pending()?;
    if saved.is_empty() {
        tracing::info!("no pending payments to recover");
        return Ok(());
    }

    let now = Utc::now();
    let mut recovered = 0u32;
    let mut expired = 0u32;

    for payment in saved {
        if payment.expires_at <= now {
            tracing::info!(order_id = %payment.order_id, "skipping expired recovered payment");
            let _ = persistence.delete_pending(&payment.order_id);
            expired += 1;
            continue;
        }
        polling_engine.register(&payment).await?;
        state
            .pending_payments
            .write()
            .await
            .insert(payment.order_id.clone(), payment);
        recovered += 1;
    }

    tracing::info!(
        recovered = recovered,
        expired_skipped = expired,
        "startup recovery complete"
    );
    Ok(())
}

/// Read incoming orders from the channel and process each one.
async fn message_listener(
    mut rx: mpsc::Receiver<IncomingOrder>,
    ctx: Arc<PipelineContext>,
) {
    while let Some(order) = rx.recv().await {
        if let Err(e) = pipeline::process_order(&ctx, &order.raw_json, &order.customer_pubkey).await
        {
            tracing::error!(
                customer = %order.customer_pubkey,
                error = %e,
                "order processing failed"
            );
        }
    }
    tracing::info!("message listener shutting down (channel closed)");
}

/// Read polling events and handle each one.
async fn event_handler(
    mut rx: mpsc::Receiver<polling::PollingEvent>,
    ctx: Arc<PipelineContext>,
) {
    while let Some(event) = rx.recv().await {
        if let Err(e) = pipeline::handle_polling_event(&ctx, event).await {
            tracing::error!(error = %e, "polling event handling failed");
        }
    }
    tracing::info!("event handler shutting down (channel closed)");
}
