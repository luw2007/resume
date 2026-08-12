//! Runtime primitives for cooperative, bounded, cancellable concurrency.
//!
//! - Bounded channels to apply backpressure between discovery and Preview.
//! - One discovery worker per integration.
//! - Maximum four Preview workers.
//! - Cooperative cancellation via a shared cancel token.
//! - 250 ms ordinary-exit join budget: workers that don't join within the
//!   budget are left to finish on their own (detached), since killing them
//!   could leave shared state inconsistent.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

/// Maximum number of concurrent Preview workers.
pub const MAX_PREVIEW_WORKERS: usize = 4;

/// Ordinary-exit join budget: workers should join within this time on normal
/// cancellation.
pub const JOIN_BUDGET: Duration = Duration::from_millis(250);

/// Maximum wall-clock time for the shared OS process-table probe.
pub const PROC_PROBE_BUDGET: Duration = Duration::from_millis(300);

/// A cooperative cancellation token. Clones share the same cancellation state.
#[derive(Clone, Debug)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cancellation to all clones.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// A bounded channel sender with backpressure. When the channel is full, sends
/// block (or fail if cancelled).
pub struct BoundedSender<T> {
    tx: std::sync::mpsc::SyncSender<T>,
    cancel: CancelToken,
}

/// A bounded channel receiver.
pub struct BoundedReceiver<T> {
    rx: std::sync::mpsc::Receiver<T>,
}

/// Create a bounded channel with the given capacity.
pub fn bounded_channel<T>(capacity: usize) -> (BoundedSender<T>, BoundedReceiver<T>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
    (
        BoundedSender {
            tx,
            cancel: CancelToken::new(),
        },
        BoundedReceiver { rx },
    )
}

/// Create a bounded channel with a shared cancel token.
pub fn bounded_channel_with_cancel<T>(
    capacity: usize,
    cancel: CancelToken,
) -> (BoundedSender<T>, BoundedReceiver<T>) {
    let (tx, rx) = std::sync::mpsc::sync_channel(capacity);
    (BoundedSender { tx, cancel }, BoundedReceiver { rx })
}

/// Outcome of a bounded send operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SendOutcome {
    /// Sent successfully.
    Sent,
    /// Cancelled before the send could complete.
    Cancelled,
    /// All receivers dropped (pipeline closed).
    Closed,
}

impl<T> BoundedSender<T> {
    /// Send a value, respecting cancellation. If the channel is full, this
    /// applies backpressure by blocking until either the value is sent,
    /// cancellation is requested, or all receivers are dropped.
    pub fn send(&self, value: T) -> SendOutcome {
        if self.cancel.is_cancelled() {
            return SendOutcome::Cancelled;
        }
        // Use try_send with a spin loop + cancellation check for backpressure.
        // The value is moved into try_send on the first attempt; on Full, the
        // value is returned back to us so we can retry.
        let mut value = Some(value);
        loop {
            if self.cancel.is_cancelled() {
                return SendOutcome::Cancelled;
            }
            match self
                .tx
                .try_send(value.take().expect("value present on loop entry"))
            {
                Ok(_) => return SendOutcome::Sent,
                Err(std::sync::mpsc::TrySendError::Full(v)) => {
                    value = Some(v);
                    thread::yield_now();
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    return SendOutcome::Closed;
                }
            }
        }
    }

    /// Signal cancellation to the shared token.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Access the shared cancel token.
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}

impl<T> BoundedReceiver<T> {
    /// Receive a value, or `None` if the channel is closed and empty.
    pub fn recv(&self) -> Option<T> {
        self.rx.recv().ok()
    }

    /// Try to receive without blocking.
    pub fn try_recv(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// Join a set of worker threads with a time budget. Workers that don't join
/// within the budget are detached (left running) rather than killed.
///
/// Returns `true` if all workers joined within the budget, `false` if some
/// were detached.
pub fn join_with_budget(handles: Vec<thread::JoinHandle<()>>, budget: Duration) -> bool {
    let deadline = std::time::Instant::now() + budget;
    let mut all_joined = true;
    for handle in handles {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            // Budget exhausted; detach remaining workers.
            all_joined = false;
            break;
        }
        match wait_for_join(handle, remaining) {
            JoinResult::Joined => {}
            JoinResult::Timeout => {
                all_joined = false;
            }
        }
    }
    all_joined
}

enum JoinResult {
    Joined,
    Timeout,
}

fn wait_for_join(handle: thread::JoinHandle<()>, timeout: Duration) -> JoinResult {
    let start = std::time::Instant::now();
    loop {
        if !handle.is_finished() {
            if start.elapsed() >= timeout {
                return JoinResult::Timeout;
            }
            thread::yield_now();
        } else {
            // The thread has finished; join it to clean up.
            // Note: we can't re-join a moved handle. Instead, since is_finished
            // is true, we know the thread exited. We just drop the handle.
            drop(handle);
            return JoinResult::Joined;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn cancel_token_signals_all_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!a.is_cancelled());
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[test]
    fn bounded_send_recv_round_trip() {
        let (tx, rx) = bounded_channel::<i32>(4);
        assert_eq!(tx.send(1), SendOutcome::Sent);
        assert_eq!(tx.send(2), SendOutcome::Sent);
        assert_eq!(rx.recv(), Some(1));
        assert_eq!(rx.recv(), Some(2));
    }

    #[test]
    fn send_returns_cancelled_after_cancel() {
        let cancel = CancelToken::new();
        let (tx, _rx) = bounded_channel_with_cancel::<i32>(1, cancel.clone());
        cancel.cancel();
        assert_eq!(tx.send(42), SendOutcome::Cancelled);
    }

    #[test]
    fn send_returns_closed_when_receiver_dropped() {
        let (tx, rx) = bounded_channel::<i32>(4);
        drop(rx);
        assert_eq!(tx.send(1), SendOutcome::Closed);
    }

    #[test]
    fn join_budget_completes_fast_workers() {
        let counter = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let c = counter.clone();
                thread::spawn(move || {
                    c.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        let all = join_with_budget(handles, JOIN_BUDGET);
        assert!(all, "fast workers should join within budget");
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn join_budget_detaches_slow_workers() {
        let handle = thread::spawn(|| {
            thread::sleep(Duration::from_secs(10));
        });
        let all = join_with_budget(vec![handle], Duration::from_millis(50));
        assert!(!all, "slow worker should be detached");
        // Note: the slow thread continues running (detached). In tests this is
        // acceptable since it's a daemon thread.
    }

    #[test]
    fn backpressure_with_full_channel() {
        let (tx, rx) = bounded_channel::<i32>(2);
        assert_eq!(tx.send(1), SendOutcome::Sent);
        assert_eq!(tx.send(2), SendOutcome::Sent);
        // Channel is now full. In a separate thread, send will block until we
        // receive.
        let tx2_tx = tx.tx.clone();
        let cancel = tx.cancel.clone();
        let handle = thread::spawn(move || {
            // Manual send with the raw sender to test blocking behavior.
            let _ = tx2_tx.send(3);
        });
        // Drain to allow the sender to proceed.
        let _ = rx.recv();
        let _ = rx.recv();
        handle.join().unwrap();
        let _ = cancel;
    }

    #[test]
    fn max_preview_workers_is_four() {
        assert_eq!(MAX_PREVIEW_WORKERS, 4);
    }

    #[test]
    fn join_budget_is_250ms() {
        assert_eq!(JOIN_BUDGET, Duration::from_millis(250));
    }
}
