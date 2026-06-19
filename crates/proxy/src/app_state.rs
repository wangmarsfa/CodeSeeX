use crate::runtime_config::RuntimeConfigService;
use crate::telemetry::TelemetryHub;
use codeseex_core::AppConfig;
use codeseex_store::Store;

#[derive(Clone)]
pub(crate) struct ProxyState {
    pub(crate) runtime_config: RuntimeConfigService,
    pub(crate) store: Store,
    pub(crate) telemetry: TelemetryHub,
}

impl ProxyState {
    pub(crate) fn new(config: AppConfig, store: Store) -> Self {
        Self {
            runtime_config: RuntimeConfigService::new(config),
            store,
            telemetry: TelemetryHub::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(config: AppConfig, store: Store) -> Self {
        Self::new(config, store)
    }

    pub(crate) fn active_config(&self) -> AppConfig {
        self.runtime_config.active_config()
    }

    pub(crate) fn client(&self) -> reqwest::Client {
        let config = self.active_config();
        let timeout = std::time::Duration::from_millis(config.upstream.timeout_ms);
        crate::network::client(config.network_proxy, timeout)
            .unwrap_or_else(|_| reqwest::Client::new())
    }
}
