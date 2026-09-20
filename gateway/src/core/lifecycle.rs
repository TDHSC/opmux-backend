//! Drain state and bounded process lifecycle for the gateway binary.
//!
//! SIGTERM and SIGINT close generation admission, mark the process unready,
//! reject later generation, and wait at most the configured shutdown grace
//! for admitted work. Grace expiry cancels remaining owned tasks. Bind
//! failures are sanitized and do not disturb an existing listener.

use super::admission::AdmissionLimiter;
use axum::Router;
use std::fmt;
use std::future::IntoFuture;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;

/// Shared drain flag for readiness and generation admission.
///
/// [`Self::mark_draining`] closes the wired [`AdmissionLimiter`] before
/// publishing the drain flag so later permit acquisition cannot admit
/// generation. Already held permits remain valid; releasing them cannot
/// reopen admission.
#[derive(Clone)]
pub struct ShutdownState {
    tx: watch::Sender<bool>,
    admission: AdmissionLimiter,
}

impl fmt::Debug for ShutdownState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShutdownState")
            .field("draining", &self.is_draining())
            .finish()
    }
}

impl ShutdownState {
    /// Creates a not-draining process state with a detached limiter.
    pub fn new() -> Self {
        Self::with_admission(AdmissionLimiter::new(0))
    }

    /// Creates drain state that closes `admission` before publishing.
    ///
    /// # Parameters
    /// - `admission` - The same limiter used by generation handlers
    ///
    /// # Returns
    /// Drain state wired to `admission`
    pub fn with_admission(admission: AdmissionLimiter) -> Self {
        let (tx, _) = watch::channel(false);
        Self { tx, admission }
    }

    /// Marks the process as draining.
    ///
    /// Closes the wired generation limiter, then publishes the drain
    /// flag. Readiness becomes unready and later acquires are denied.
    /// Already admitted work may continue until shutdown grace expires.
    pub fn mark_draining(&self) {
        self.admission.close();
        self.tx.send_replace(true);
    }

    /// Returns whether shutdown draining has started.
    pub fn is_draining(&self) -> bool {
        *self.tx.borrow()
    }

    /// Waits until [`Self::mark_draining`] has been called.
    pub async fn wait_until_draining(&self) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

impl Default for ShutdownState {
    fn default() -> Self {
        Self::new()
    }
}

/// Sanitized listen/bind failure for process startup.
#[derive(Debug)]
pub struct ListenError {
    category: &'static str,
    message: &'static str,
}

impl ListenError {
    fn in_use() -> Self {
        Self {
            category: "bind_address_in_use",
            message: "failed to bind the listen address because it is already in use",
        }
    }

    fn failed() -> Self {
        Self {
            category: "listen_failed",
            message: "failed to bind the configured listen address",
        }
    }

    /// Stable diagnostic category. Safe to log and assert.
    pub fn category(&self) -> &'static str {
        self.category
    }
}

impl fmt::Display for ListenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category, self.message)
    }
}

impl std::error::Error for ListenError {}

/// Binds the configured listen address.
///
/// Address-in-use fails with a sanitized category and does not replace or
/// terminate the process that already owns the port.
///
/// # Parameters
/// - `addr` - Configured bind address
///
/// # Returns
/// Bound listener
///
/// # Errors
/// Returns [`ListenError`] when the address cannot be bound.
pub async fn bind_listener(addr: SocketAddr) -> Result<TcpListener, ListenError> {
    match TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == ErrorKind::AddrInUse => Err(ListenError::in_use()),
        Err(_) => Err(ListenError::failed()),
    }
}

/// Serves `app` until SIGTERM/SIGINT, then drains for at most `grace`.
///
/// # Parameters
/// - `listener` - Bound listen socket
/// - `app` - Production router
/// - `shutdown` - Shared drain flag marked when a shutdown signal arrives
/// - `grace` - Maximum time to wait for admitted work after the signal
///
/// # Returns
/// `Ok(())` when the server stops cleanly or grace expires
///
/// # Errors
/// Returns the server I/O error when the listener fails after bind
pub async fn serve_until_shutdown(
    listener: TcpListener,
    app: Router,
    shutdown: ShutdownState,
    grace: Duration,
) -> std::io::Result<()> {
    let _signal_handler = spawn_signal_handler(shutdown.clone());

    let drain_started = shutdown.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            drain_started.wait_until_draining().await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result,
        _ = async {
            shutdown.wait_until_draining().await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::warn!(
                category = "shutdown_grace_elapsed",
                "shutdown grace elapsed; cancelling remaining work"
            );
            Ok(())
        }
    }
}

fn spawn_signal_handler(shutdown: ShutdownState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                Ok(mut sigterm) => {
                    tokio::select! {
                        result = tokio::signal::ctrl_c() => {
                            if let Err(error) = result {
                                tracing::error!(
                                    category = "signal_handler",
                                    error = %error,
                                    "failed to listen for SIGINT"
                                );
                            } else {
                                tracing::info!(signal = "SIGINT", "received shutdown signal");
                            }
                        }
                        _ = sigterm.recv() => {
                            tracing::info!(signal = "SIGTERM", "received shutdown signal");
                        }
                    }
                    tracing::info!("shutdown signal received; draining");
                    shutdown.mark_draining();
                    loop {
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {}
                            _ = sigterm.recv() => {}
                        }
                    }
                }
                Err(error) => {
                    tracing::error!(
                        category = "signal_handler",
                        error = %error,
                        "failed to install SIGTERM handler"
                    );
                    let _ = tokio::signal::ctrl_c().await;
                    tracing::info!(signal = "SIGINT", "received shutdown signal");
                    shutdown.mark_draining();
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!(signal = "SIGINT", "received shutdown signal");
            shutdown.mark_draining();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::admission::{AdmissionDenied, AdmissionLimiter};
    use std::time::Instant;

    #[tokio::test]
    async fn mark_draining_wakes_waiters() {
        let shutdown = ShutdownState::new();
        assert!(!shutdown.is_draining());
        let waiter = shutdown.clone();
        let handle = tokio::spawn(async move {
            waiter.wait_until_draining().await;
        });
        shutdown.mark_draining();
        handle.await.expect("waiter");
        assert!(shutdown.is_draining());
        shutdown.wait_until_draining().await;
    }

    #[tokio::test]
    async fn occupied_address_fails_without_taking_the_port() {
        let holder = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let addr = holder.local_addr().expect("fixture addr");
        let error = bind_listener(addr).await.expect_err("occupied port");
        assert_eq!(error.category(), "bind_address_in_use");
        assert!(!error.to_string().contains("postgres://"));
        assert!(!error.to_string().contains("Bearer"));
        let still_ours = holder.local_addr().expect("owner still bound");
        assert_eq!(still_ours, addr);
    }

    #[tokio::test]
    async fn wait_until_draining_is_immediate_when_already_set() {
        let shutdown = ShutdownState::new();
        shutdown.mark_draining();
        let started = Instant::now();
        shutdown.wait_until_draining().await;
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn mark_draining_closes_wired_limiter_before_publishing() {
        let admission = AdmissionLimiter::new(1);
        let shutdown = ShutdownState::with_admission(admission.clone());
        let predating = admission.try_acquire().expect("predating permit");
        assert!(!shutdown.is_draining());
        shutdown.mark_draining();
        assert!(shutdown.is_draining());
        assert!(matches!(
            admission.try_acquire(),
            Err(AdmissionDenied::Closed)
        ));
        drop(predating);
        assert!(matches!(
            admission.try_acquire(),
            Err(AdmissionDenied::Closed)
        ));
    }
}
