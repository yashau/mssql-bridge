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
        let bytes = std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("reading config {}: {e}", path.display()))?;
        let raw = decode_text(&bytes)
            .map_err(|e| anyhow::anyhow!("decoding config {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("parsing config {}: {e}", path.display()))?;
        Ok(cfg)
    }
}

/// Decode bytes as text, tolerating the BOMs Windows tools tend to write:
/// UTF-8 with BOM (Notepad's default since Win10 1903, PowerShell 7's
/// Out-File -Encoding utf8BOM), and UTF-16 LE/BE with BOM (Windows PowerShell
/// 5's `>` redirect). Without a BOM, bytes are read as UTF-8.
fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(bytes[3..].to_vec())
            .map_err(|e| format!("file has UTF-8 BOM but body is not valid UTF-8: {e}"));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return decode_utf16(&bytes[2..], u16::from_le_bytes)
            .map_err(|e| format!("file has UTF-16 LE BOM but body is invalid: {e}"));
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return decode_utf16(&bytes[2..], u16::from_be_bytes)
            .map_err(|e| format!("file has UTF-16 BE BOM but body is invalid: {e}"));
    }
    String::from_utf8(bytes.to_vec()).map_err(|e| format!("file is not valid UTF-8: {e}"))
}

fn decode_utf16(bytes: &[u8], to_u16: fn([u8; 2]) -> u16) -> Result<String, String> {
    if bytes.len() % 2 != 0 {
        return Err("odd byte length for UTF-16".into());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| to_u16([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::decode_text;

    #[test]
    fn plain_utf8() {
        assert_eq!(decode_text(b"hello").unwrap(), "hello");
    }

    #[test]
    fn utf8_with_bom() {
        let mut v = vec![0xEF, 0xBB, 0xBF];
        v.extend_from_slice(b"hello");
        assert_eq!(decode_text(&v).unwrap(), "hello");
    }

    #[test]
    fn utf16_le_with_bom() {
        // "hi"
        let v = [0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        assert_eq!(decode_text(&v).unwrap(), "hi");
    }

    #[test]
    fn utf16_be_with_bom() {
        let v = [0xFE, 0xFF, 0x00, b'h', 0x00, b'i'];
        assert_eq!(decode_text(&v).unwrap(), "hi");
    }
}
