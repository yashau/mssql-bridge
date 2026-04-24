use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub mssql: MssqlConfig,
    #[serde(default)]
    pub pool: PoolConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub request_timeout_secs: u64,
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:3001".parse().unwrap(),
            request_timeout_secs: 120,
            max_body_bytes: 1_048_576,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MssqlConfig {
    pub host: String,
    pub port: u16,
    /// Named instance (e.g. "SQLEXPRESS"). When set, the port is discovered via
    /// SQL Server Browser on UDP 1434 and the `port` field is ignored.
    pub instance_name: Option<String>,
    pub default_database: Option<String>,
    pub encryption: Encryption,
    pub trust_server_certificate: bool,
    pub application_name: String,
}

impl Default for MssqlConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 1433,
            instance_name: None,
            default_database: None,
            encryption: Encryption::Required,
            trust_server_certificate: false,
            application_name: "mssql-bridge".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Encryption {
    Off,
    On,
    Required,
    NotSupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    pub max_connections_per_credential: u32,
    pub min_idle: u32,
    pub connection_timeout_secs: u64,
    pub idle_timeout_secs: u64,
    pub max_credential_pools: u64,
    pub credential_ttl_secs: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections_per_credential: 10,
            min_idle: 0,
            connection_timeout_secs: 10,
            idle_timeout_secs: 300,
            max_credential_pools: 64,
            credential_ttl_secs: 900,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LimitsConfig {
    pub max_rows: usize,
    pub query_timeout_secs: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_rows: 100_000,
            query_timeout_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogConfig {
    pub level: String,
    pub log_sql: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: "info".into(),
            log_sql: false,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))?;
        Ok(cfg)
    }
}
