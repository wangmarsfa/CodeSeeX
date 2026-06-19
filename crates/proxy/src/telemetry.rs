use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const TELEMETRY_EVENT_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TelemetryEventKind {
    RequestStarted,
    UpstreamCallStarted,
    UpstreamCallCompleted,
    ToolCallStarted,
    ToolCallCompleted,
    RequestCompleted,
    RequestFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TelemetryEvent {
    pub(crate) kind: TelemetryEventKind,
    pub(crate) request_id: Option<String>,
    pub(crate) phase: Option<String>,
    pub(crate) detail: Value,
}

impl TelemetryEvent {
    pub(crate) fn new(
        kind: TelemetryEventKind,
        request_id: impl Into<Option<String>>,
        phase: impl Into<Option<String>>,
        detail: Value,
    ) -> Self {
        Self {
            kind,
            request_id: request_id.into(),
            phase: phase.into(),
            detail,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub(crate) struct TelemetryLedgerSnapshot {
    pub(crate) events: Vec<TelemetryEvent>,
}

#[derive(Clone)]
pub(crate) struct TelemetryHub {
    events: Arc<Mutex<VecDeque<TelemetryEvent>>>,
    tx: broadcast::Sender<TelemetryEvent>,
}

impl TelemetryHub {
    pub(crate) fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            events: Arc::new(Mutex::new(VecDeque::new())),
            tx,
        }
    }

    pub(crate) fn emit(&self, event: TelemetryEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push_back(event.clone());
            while events.len() > TELEMETRY_EVENT_CAPACITY {
                events.pop_front();
            }
        }
        let _ = self.tx.send(event);
    }

    #[allow(dead_code)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<TelemetryEvent> {
        self.tx.subscribe()
    }

    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> TelemetryLedgerSnapshot {
        let events = self
            .events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default();
        TelemetryLedgerSnapshot { events }
    }

    pub(crate) fn emit_framework_started(&self) {
        self.emit(TelemetryEvent::new(
            TelemetryEventKind::RequestStarted,
            None,
            Some("telemetry_framework".to_owned()),
            json!({
                "note": "Telemetry framework is available but usage/logs still use the existing store paths."
            }),
        ));
    }
}

impl Default for TelemetryHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_hub_records_and_broadcasts_events() {
        let hub = TelemetryHub::new();
        let mut rx = hub.subscribe();
        hub.emit(TelemetryEvent::new(
            TelemetryEventKind::RequestStarted,
            Some("resp_test".to_owned()),
            Some("start".to_owned()),
            json!({ "ok": true }),
        ));

        assert_eq!(hub.snapshot().events.len(), 1);
        let received = rx.try_recv().expect("broadcast event");
        assert_eq!(received.request_id.as_deref(), Some("resp_test"));
    }
}
