use std::sync::Arc;
use std::time::Duration;

use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use moka::future::Cache;
use tiberius::{AuthMethod, Config as TiberiusConfig, EncryptionLevel};

use crate::config::{Encryption, MssqlConfig, PoolConfig};
use crate::error::BridgeError;

pub type DbPool = Pool<ConnectionManager>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialKey {
    pub user: String,
    pub password: String,
    pub database: String,
}

pub struct PoolManager {
    mssql: MssqlConfig,
    pool_cfg: PoolConfig,
    cache: Cache<CredentialKey, Arc<DbPool>>,
}

impl PoolManager {
    pub fn new(mssql: MssqlConfig, pool_cfg: PoolConfig) -> Self {
        let cache = Cache::builder()
            .max_capacity(pool_cfg.max_credential_pools)
            .time_to_idle(Duration::from_secs(pool_cfg.credential_ttl_secs))
            .build();
        Self {
            mssql,
            pool_cfg,
            cache,
        }
    }

    pub async fn get(&self, key: CredentialKey) -> Result<Arc<DbPool>, BridgeError> {
        if let Some(existing) = self.cache.get(&key).await {
            return Ok(existing);
        }

        let tds = self.build_tiberius_config(&key);
        let mgr = ConnectionManager::build(tds)
            .map_err(|e| BridgeError::Pool(format!("build connection manager: {e}")))?;

        let pool = Pool::builder()
            .max_size(self.pool_cfg.max_connections_per_credential)
            .min_idle(self.pool_cfg.min_idle)
            .connection_timeout(Duration::from_secs(self.pool_cfg.connection_timeout_secs))
            .idle_timeout(Some(Duration::from_secs(self.pool_cfg.idle_timeout_secs)))
            .build(mgr)
            .await
            .map_err(|e| BridgeError::Pool(e.to_string()))?;

        let arc_pool = Arc::new(pool);
        self.cache.insert(key, arc_pool.clone()).await;
        Ok(arc_pool)
    }

    fn build_tiberius_config(&self, key: &CredentialKey) -> TiberiusConfig {
        let mut cfg = TiberiusConfig::new();
        cfg.host(&self.mssql.host);
        if let Some(instance) = &self.mssql.instance_name {
            cfg.instance_name(instance);
        } else {
            cfg.port(self.mssql.port);
        }
        cfg.database(&key.database);
        cfg.authentication(AuthMethod::sql_server(&key.user, &key.password));
        cfg.application_name(&self.mssql.application_name);
        cfg.encryption(match self.mssql.encryption {
            Encryption::Off => EncryptionLevel::Off,
            Encryption::On => EncryptionLevel::On,
            Encryption::Required => EncryptionLevel::Required,
            Encryption::NotSupported => EncryptionLevel::NotSupported,
        });
        if self.mssql.trust_server_certificate {
            cfg.trust_cert();
        }
        cfg
    }
}
