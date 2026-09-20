//! Recoverable loopback TCP proxy for same-instance database outage tests.
//!
//! Forwards `127.0.0.1:<assigned>` to the owned Supabase listener. Pause closes
//! existing connections and refuses new handshakes; resume restores forwarding
//! on the same listen port without rebuilding callers.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::copy;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{AbortHandle, JoinHandle};

const OWNED_UPSTREAM: &str = "127.0.0.1:55432";

/// Loopback TCP proxy that can interrupt and restore Postgres connectivity.
pub struct RecoverableDbProxy {
    listen_addr: SocketAddr,
    forwarding: Arc<AtomicBool>,
    active: Arc<Mutex<Vec<AbortHandle>>>,
    accept_loop: JoinHandle<()>,
}

impl RecoverableDbProxy {
    /// Binds `127.0.0.1:0` and forwards to the owned database.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("proxy must bind loopback");
        let listen_addr = listener.local_addr().expect("proxy addr");
        let forwarding = Arc::new(AtomicBool::new(true));
        let active = Arc::new(Mutex::new(Vec::new()));
        let accept_loop =
            tokio::spawn(accept_loop(listener, forwarding.clone(), active.clone()));
        Self {
            listen_addr,
            forwarding,
            active,
            accept_loop,
        }
    }

    /// Returns the loopback port callers should dial.
    pub fn port(&self) -> u16 {
        self.listen_addr.port()
    }

    /// Drops existing forwarded connections and rejects new handshakes.
    pub fn pause(&self) {
        self.forwarding.store(false, Ordering::SeqCst);
        abort_active(&self.active);
    }

    /// Resumes forwarding on the original listen socket.
    pub fn resume(&self) {
        self.forwarding.store(true, Ordering::SeqCst);
    }
}

impl Drop for RecoverableDbProxy {
    fn drop(&mut self) {
        self.pause();
        self.accept_loop.abort();
    }
}

fn abort_active(active: &Arc<Mutex<Vec<AbortHandle>>>) {
    let mut handles = active.lock().expect("proxy tasks");
    for handle in handles.drain(..) {
        handle.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    forwarding: Arc<AtomicBool>,
    active: Arc<Mutex<Vec<AbortHandle>>>,
) {
    loop {
        let Ok((inbound, _)) = listener.accept().await else {
            continue;
        };
        if !forwarding.load(Ordering::SeqCst) {
            drop(inbound);
            continue;
        }
        let handle = tokio::spawn(forward(inbound));
        active
            .lock()
            .expect("proxy tasks")
            .push(handle.abort_handle());
    }
}

async fn forward(inbound: TcpStream) {
    let Ok(upstream) = TcpStream::connect(OWNED_UPSTREAM).await else {
        return;
    };
    let _ = pump(inbound, upstream).await;
}

async fn pump(mut left: TcpStream, mut right: TcpStream) {
    let (mut left_read, mut left_write) = left.split();
    let (mut right_read, mut right_write) = right.split();
    tokio::select! {
        _ = copy(&mut left_read, &mut right_write) => {}
        _ = copy(&mut right_read, &mut left_write) => {}
    }
}
