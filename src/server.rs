use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, Response, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::{info, warn, Level};

use crate::auth::BasicCredentials;
use crate::config::Config;
use crate::error::BridgeError;
use crate::pool::{CredentialKey, PoolManager};
use crate::query::{self, QueryRequest, QueryResponse};

#[derive(Clone)]
pub struct AppState {
    pub pools: Arc<PoolManager>,
    pub config: Arc<Config>,
}

pub fn router(state: AppState) -> Router {
    let body_limit = state.config.server.max_body_bytes;
    let timeout = Duration::from_secs(state.config.server.request_timeout_secs);

    let buffered = Router::new()
        .route("/query", post(handle_query))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            timeout,
        ));

    let streaming = Router::new().route("/query/stream", post(handle_query_stream));

    Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(buffered)
        .merge(streaming)
        .layer(RequestBodyLimitLayer::new(body_limit))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}

pub async fn serve<F>(state: AppState, bind: SocketAddr, shutdown: F) -> anyhow::Result<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let app = router(state);
    info!(%bind, "listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Foreground shutdown future. Waits for Ctrl-C on Windows, or
/// SIGTERM/SIGINT on Unix. Intended for console use; the Windows service
/// path uses its own SCM-driven oneshot instead.
pub async fn ctrl_c_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        tokio::select! {
            _ = term.recv() => {},
            _ = int.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    info!("shutdown signal received");
}

#[derive(Debug, Deserialize, Default)]
pub struct QueryParams {
    /// When true, each row is emitted as a JSON object keyed by column name.
    /// Default: false (rows are emitted as positional arrays).
    #[serde(default)]
    pub rows_as_objects: bool,
}

fn resolve_database(req: &QueryRequest, cfg: &Config) -> Result<String, BridgeError> {
    req.database
        .clone()
        .or_else(|| cfg.mssql.default_database.clone())
        .ok_or(BridgeError::NoDatabase)
}

async fn handle_query(
    State(state): State<AppState>,
    creds: BasicCredentials,
    Query(params): Query<QueryParams>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, BridgeError> {
    let database = resolve_database(&req, &state.config)?;
    let key = CredentialKey {
        user: creds.user,
        password: creds.password,
        database,
    };

    if state.config.log.log_sql {
        info!(sql = %req.sql, params = req.params.len(), "query");
    }

    let pool = state.pools.get(key).await?;
    let mut conn = pool
        .get()
        .await
        .map_err(|e| BridgeError::Pool(e.to_string()))?;

    let resp = query::execute(
        &mut conn,
        &req,
        state.config.limits.query_timeout_secs,
        state.config.limits.max_rows,
        params.rows_as_objects,
    )
    .await?;

    Ok(Json(resp))
}

async fn handle_query_stream(
    State(state): State<AppState>,
    creds: BasicCredentials,
    Query(params): Query<QueryParams>,
    Json(req): Json<QueryRequest>,
) -> Result<Response<Body>, BridgeError> {
    let database = resolve_database(&req, &state.config)?;
    let key = CredentialKey {
        user: creds.user,
        password: creds.password,
        database,
    };

    if state.config.log.log_sql {
        info!(sql = %req.sql, params = req.params.len(), "query/stream");
    }

    let pool = state.pools.get(key).await?;

    // Bounded channel provides backpressure: tiberius pauses pulling rows
    // when the client reads slowly.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(256);

    tokio::spawn(async move {
        let tx2 = tx.clone();
        let emit = move |chunk: bytes::Bytes| {
            let tx = tx2.clone();
            async move { tx.send(Ok(chunk)).await.is_ok() }
        };

        if let Err(e) = query::stream_execute(pool, req, params.rows_as_objects, emit).await {
            let frame = serde_json::json!({
                "type": "error",
                "message": e.to_string(),
            });
            let mut buf = serde_json::to_vec(&frame).unwrap_or_default();
            buf.push(b'\n');
            let _ = tx.send(Ok(bytes::Bytes::from(buf))).await;
            warn!(error = %e, "stream aborted");
        }
    });

    let body = Body::from_stream(ReceiverStream::new(rx));
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/x-ndjson"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );

    let mut resp = Response::new(body);
    *resp.status_mut() = StatusCode::OK;
    *resp.headers_mut() = headers;
    Ok(resp)
}

pub fn init_logging(level: &str) -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_new(level)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .init();
    Ok(())
}

impl AppState {
    pub fn from_config(cfg: Config) -> Self {
        let pools = Arc::new(PoolManager::new(cfg.mssql.clone(), cfg.pool.clone()));
        Self {
            pools,
            config: Arc::new(cfg),
        }
    }
}
