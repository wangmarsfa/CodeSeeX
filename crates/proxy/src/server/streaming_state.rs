use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tokio::sync::Notify;

type StreamingCancellationMap = BTreeMap<String, StreamingCancellation>;

static STREAMING_CANCELLATIONS: OnceLock<Mutex<StreamingCancellationMap>> = OnceLock::new();

#[derive(Clone)]
pub(super) struct StreamingCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl StreamingCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub(super) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

fn streaming_cancellations() -> &'static Mutex<StreamingCancellationMap> {
    STREAMING_CANCELLATIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn register_streaming_response(response_id: &str) -> StreamingCancellation {
    let cancelled = StreamingCancellation::new();
    if let Ok(mut active) = streaming_cancellations().lock() {
        active.insert(response_id.to_owned(), cancelled.clone());
    }
    cancelled
}

pub(super) fn unregister_streaming_response(response_id: &str) {
    if let Ok(mut active) = streaming_cancellations().lock() {
        active.remove(response_id);
    }
}

pub(super) fn cancel_streaming_response(response_id: &str) -> bool {
    let Ok(active) = streaming_cancellations().lock() else {
        return false;
    };
    let Some(cancelled) = active.get(response_id) else {
        return false;
    };
    cancelled.cancel();
    true
}

pub(super) fn streaming_response_cancelled(cancelled: &StreamingCancellation) -> bool {
    cancelled.is_cancelled()
}

pub(super) struct StreamingRequestGuard {
    store: codeseex_store::Store,
    response_id: String,
    cancelled: StreamingCancellation,
}

impl StreamingRequestGuard {
    pub(super) fn new(
        store: codeseex_store::Store,
        response_id: String,
        cancelled: StreamingCancellation,
    ) -> Self {
        Self {
            store,
            response_id,
            cancelled,
        }
    }
}

impl Drop for StreamingRequestGuard {
    fn drop(&mut self) {
        self.cancelled.cancel();
        unregister_streaming_response(&self.response_id);
        let store = self.store.clone();
        let response_id = self.response_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = store
                    .interrupt_request_if_in_progress(
                        &response_id,
                        "stream dropped before request completion",
                    )
                    .await;
            });
        }
    }
}
