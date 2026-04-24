mod auth;
mod config;
mod error;
mod pool;
mod query;
mod server;
mod service;

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::config::{Config, Encryption};

const LONG_ABOUT: &str = "\
A thin HTTP-to-MSSQL proxy.

Runs as a foreground process or a Windows service. Speaks a small JSON protocol;
routes parameterized SQL straight to SQL Server and returns rows. Authentication
is delegated to SQL Server: each request presents Basic Auth credentials that are
used to open a connection.

Quick start with defaults (connects to localhost:1433, listens on 127.0.0.1:3001):
    mssql-bridge

Override individual settings on the command line:
    mssql-bridge --bind 0.0.0.0:3001 --mssql-host db.local --default-database MyDb

Load a config.toml and still override specific fields:
    mssql-bridge --config ./config.toml --bind 0.0.0.0:8080

Generate a starter config.toml:
    mssql-bridge print-config > config.toml
";

#[derive(Parser, Debug)]
#[command(
    name = "mssql-bridge",
    version,
    about = "HTTP-to-MSSQL proxy",
    long_about = LONG_ABOUT,
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    #[command(flatten)]
    overrides: Overrides,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the server in the foreground (default when no subcommand given).
    Run,

    /// Run as a Windows service (invoked by the Service Control Manager).
    #[command(hide = true)]
    ServiceRun,

    /// Install as a Windows service set to auto-start.
    #[cfg(windows)]
    Install {
        /// Path to the config.toml the installed service should load.
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },

    /// Uninstall the Windows service.
    #[cfg(windows)]
    Uninstall,

    /// Print a default config.toml to stdout.
    PrintConfig,
}

#[derive(Args, Debug, Clone, Default)]
struct Overrides {
    /// Path to config.toml. Overrides still apply on top of it.
    #[arg(long, short, value_name = "PATH", env = "MSSQL_BRIDGE_CONFIG")]
    config: Option<PathBuf>,

    /// HTTP listen address. Default: 127.0.0.1:3001
    #[arg(long, value_name = "HOST:PORT", help_heading = "Server")]
    bind: Option<SocketAddr>,

    /// Overall HTTP request timeout in seconds (buffered /query only).
    #[arg(long, value_name = "SECS", help_heading = "Server")]
    request_timeout: Option<u64>,

    /// Max request body size in bytes.
    #[arg(long, value_name = "BYTES", help_heading = "Server")]
    max_body_bytes: Option<usize>,

    /// SQL Server hostname. Default: localhost
    #[arg(long, value_name = "HOST", help_heading = "SQL Server")]
    mssql_host: Option<String>,

    /// SQL Server port. Ignored when --mssql-instance is set. Default: 1433
    #[arg(long, value_name = "PORT", help_heading = "SQL Server")]
    mssql_port: Option<u16>,

    /// Named instance (e.g. SQLEXPRESS). Port is discovered via SQL Browser on UDP 1434.
    #[arg(long, value_name = "NAME", help_heading = "SQL Server")]
    mssql_instance: Option<String>,

    /// Default database when a request does not specify one.
    #[arg(long, value_name = "DB", help_heading = "SQL Server")]
    default_database: Option<String>,

    /// TLS mode for the SQL Server connection. Default: required
    #[arg(long, value_enum, value_name = "MODE", help_heading = "SQL Server")]
    encryption: Option<EncryptionArg>,

    /// Accept self-signed SQL Server certificates.
    #[arg(long, action = ArgAction::SetTrue, help_heading = "SQL Server")]
    trust_server_certificate: bool,

    /// Application name reported to SQL Server.
    #[arg(long, value_name = "NAME", help_heading = "SQL Server")]
    application_name: Option<String>,

    /// Max rows returned by /query (buffered). /query/stream is unbounded.
    #[arg(long, value_name = "N", help_heading = "Limits")]
    max_rows: Option<usize>,

    /// Per-query timeout in seconds enforced inside SQL Server execution.
    #[arg(long, value_name = "SECS", help_heading = "Limits")]
    query_timeout: Option<u64>,

    /// Max concurrent connections per (user, password, database) tuple.
    #[arg(long, value_name = "N", help_heading = "Pool")]
    pool_size: Option<u32>,

    /// Idle connections kept warm per credential pool.
    #[arg(long, value_name = "N", help_heading = "Pool")]
    pool_min_idle: Option<u32>,

    /// Log level: trace | debug | info | warn | error
    #[arg(long, value_name = "LEVEL", help_heading = "Logging")]
    log_level: Option<String>,

    /// Log SQL text (never parameter values).
    #[arg(long, action = ArgAction::SetTrue, help_heading = "Logging")]
    log_sql: bool,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum EncryptionArg {
    Off,
    On,
    Required,
    #[value(name = "not-supported")]
    NotSupported,
}

impl From<EncryptionArg> for Encryption {
    fn from(v: EncryptionArg) -> Self {
        match v {
            EncryptionArg::Off => Encryption::Off,
            EncryptionArg::On => Encryption::On,
            EncryptionArg::Required => Encryption::Required,
            EncryptionArg::NotSupported => Encryption::NotSupported,
        }
    }
}

fn build_config(overrides: &Overrides) -> anyhow::Result<Config> {
    let mut cfg = match &overrides.config {
        Some(p) => Config::load(p)?,
        None => Config::default(),
    };

    if let Some(v) = overrides.bind {
        cfg.server.bind = v;
    }
    if let Some(v) = overrides.request_timeout {
        cfg.server.request_timeout_secs = v;
    }
    if let Some(v) = overrides.max_body_bytes {
        cfg.server.max_body_bytes = v;
    }

    if let Some(v) = &overrides.mssql_host {
        cfg.mssql.host = v.clone();
    }
    if let Some(v) = overrides.mssql_port {
        cfg.mssql.port = v;
    }
    if overrides.mssql_instance.is_some() {
        cfg.mssql.instance_name = overrides.mssql_instance.clone();
    }
    if overrides.default_database.is_some() {
        cfg.mssql.default_database = overrides.default_database.clone();
    }
    if let Some(v) = overrides.encryption {
        cfg.mssql.encryption = v.into();
    }
    if overrides.trust_server_certificate {
        cfg.mssql.trust_server_certificate = true;
    }
    if let Some(v) = &overrides.application_name {
        cfg.mssql.application_name = v.clone();
    }

    if let Some(v) = overrides.max_rows {
        cfg.limits.max_rows = v;
    }
    if let Some(v) = overrides.query_timeout {
        cfg.limits.query_timeout_secs = v;
    }

    if let Some(v) = overrides.pool_size {
        cfg.pool.max_connections_per_credential = v;
    }
    if let Some(v) = overrides.pool_min_idle {
        cfg.pool.min_idle = v;
    }

    if let Some(v) = &overrides.log_level {
        cfg.log.level = v.clone();
    }
    if overrides.log_sql {
        cfg.log.log_sql = true;
    }

    Ok(cfg)
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => {
            let cfg = build_config(&cli.overrides)?;
            run_foreground(cfg)
        }

        Cmd::ServiceRun => {
            #[cfg(windows)]
            {
                service::windows::dispatch()
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("service-run is only available on Windows")
            }
        }

        #[cfg(windows)]
        Cmd::Install { config } => {
            let exe = std::env::current_exe()?;
            service::windows::install(exe, config)?;
            println!("Installed Windows service 'mssql-bridge'.");
            println!("Start with: sc.exe start mssql-bridge");
            Ok(())
        }

        #[cfg(windows)]
        Cmd::Uninstall => {
            service::windows::uninstall()?;
            println!("Uninstalled Windows service 'mssql-bridge'.");
            Ok(())
        }

        Cmd::PrintConfig => {
            let c = Config::default();
            let s = toml::to_string_pretty(&c)?;
            print!("{s}");
            Ok(())
        }
    }
}

fn run_foreground(cfg: Config) -> anyhow::Result<()> {
    server::init_logging(&cfg.log.level)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let state = server::AppState::from_config(cfg.clone());
        server::serve(state, cfg.server.bind).await
    })
}
