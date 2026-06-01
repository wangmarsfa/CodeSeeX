use codeseex_core::{AppConfig, UserConfig};
use codeseex_store::Store;
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct ProxyState {
    pub(crate) config: Arc<AppConfig>,
    pub(crate) client: reqwest::Client,
    pub(crate) store: Store,
}

impl ProxyState {
    pub(crate) fn active_config(&self) -> AppConfig {
        let mut config = self.config.as_ref().clone();
        if let Ok(user_config) = UserConfig::read_from(&config.config_path()) {
            config.apply_user_config(user_config);
        }
        config
    }
}
