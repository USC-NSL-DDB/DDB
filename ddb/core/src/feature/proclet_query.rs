use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tracing::debug;

use crate::{
    common::Config,
    feature::proclet_ctrl::{ProcletCtrlClient, QueryProcletResp},
    plugin::FrameworkPlugin,
};

/// Application service for querying the framework proclet controller.
///
/// Keeping this capability separate from DbgManager removes the construction
/// cycle between debugger lifecycle ownership and command-domain services.
pub(crate) struct ProcletQueryService {
    client: Option<ProcletCtrlClient>,
}

impl ProcletQueryService {
    pub(crate) async fn connect(
        config: &Config,
        plugin: &dyn FrameworkPlugin,
    ) -> Result<Arc<Self>> {
        let client = if plugin.supports_migration(config) {
            debug!("Migration support is ENABLED, initializing proxy proclet controller.");
            Some(
                ProcletCtrlClient::try_connect_default()
                    .await
                    .context("failed to connect to proclet controller")?,
            )
        } else {
            debug!("Migration support is DISABLED; skipping proclet controller.");
            None
        };
        Ok(Arc::new(Self { client }))
    }

    pub(crate) async fn query(&self, proclet_id: u64) -> Result<QueryProcletResp> {
        let Some(client) = &self.client else {
            bail!("Proclet controller not available.");
        };
        let response = client.query_proclet(proclet_id).await?;
        debug_assert_eq!(response.proclet_id, proclet_id);
        Ok(response)
    }
}
