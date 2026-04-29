use std::{net::IpAddr, sync::Arc};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
};
use mcp::task_server::McpServer;
use rmcp::{
    ServiceExt,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, prelude::*};
use utils::{
    port_file::read_port_file,
    sentry::{self as sentry_utils, SentrySource, sentry_layer},
};

const HOST_ENV: &str = "MCP_HOST";
const PORT_ENV: &str = "MCP_PORT";

const HTTP_HOST_ENV: &str = "VIBE_KANBAN_MCP_HTTP_HOST";
const HTTP_PORT_ENV: &str = "VIBE_KANBAN_MCP_HTTP_PORT";
const HTTP_TOKEN_ENV: &str = "VIBE_KANBAN_MCP_HTTP_TOKEN";
const TRANSPORT_ENV: &str = "VIBE_KANBAN_MCP_TRANSPORT";

const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
const DEFAULT_HTTP_PORT: u16 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpLaunchMode {
    Global,
    Orchestrator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Transport {
    Stdio,
    Http(HttpConfig),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HttpConfig {
    host: String,
    port: u16,
    token: Option<String>,
    stateless: bool,
    json_response: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchConfig {
    mode: McpLaunchMode,
    transport: Transport,
}

fn main() -> anyhow::Result<()> {
    let launch_config = resolve_launch_config()?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let version = env!("CARGO_PKG_VERSION");
            init_process_logging("vibe-kanban-mcp", version);

            let base_url = resolve_base_url("vibe-kanban-mcp").await?;
            let LaunchConfig { mode, transport } = launch_config;

            let server = match mode {
                McpLaunchMode::Global => McpServer::new_global(&base_url),
                McpLaunchMode::Orchestrator => McpServer::new_orchestrator(&base_url),
            };

            let server = server.init().await?;

            match transport {
                Transport::Stdio => serve_stdio(server).await,
                Transport::Http(http_config) => serve_http(server, http_config).await,
            }
        })
}

async fn serve_stdio(server: McpServer) -> anyhow::Result<()> {
    let service = server.serve(stdio()).await.map_err(|error| {
        tracing::error!("serving error: {:?}", error);
        error
    })?;

    service.waiting().await?;
    Ok(())
}

async fn serve_http(server: McpServer, config: HttpConfig) -> anyhow::Result<()> {
    let HttpConfig {
        host,
        port,
        token,
        stateless,
        json_response,
    } = config;

    if !host_is_loopback(&host) && token.is_none() {
        tracing::warn!(
            "vibe-kanban-mcp HTTP transport is binding to non-loopback address {host} without a \
             token. Anyone who can reach this port can call MCP tools that mutate Vibe Kanban \
             state. Set --http-token (or {HTTP_TOKEN_ENV}) or put a reverse proxy in front."
        );
    }

    let cancellation_token = CancellationToken::new();

    let mut http_config = StreamableHttpServerConfig::default();
    http_config.stateful_mode = !stateless;
    http_config.json_response = json_response;
    http_config.cancellation_token = cancellation_token.child_token();

    let service = StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(server.clone()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );

    let mut router = axum::Router::new().nest_service("/mcp", service);
    if let Some(token) = token {
        let expected = Arc::new(format!("Bearer {}", token));
        router = router.layer(middleware::from_fn(move |req, next| {
            let expected = expected.clone();
            require_bearer(expected, req, next)
        }));
    }

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .map_err(|e| anyhow::anyhow!("Failed to bind {host}:{port}: {e}"))?;
    let local_addr = listener.local_addr()?;
    tracing::info!(
        "vibe-kanban-mcp HTTP transport listening on http://{local_addr}/mcp (stateful={}, \
         json_response={})",
        !stateless,
        json_response,
    );

    let shutdown = cancellation_token.clone();
    let serve = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown.cancel();
    });

    serve.await?;
    Ok(())
}

/// Best-effort check for "this host name only resolves to loopback".
///
/// Used to decide whether to emit the unauthenticated-binding warning. We
/// recognise IP literals via `IpAddr::is_loopback`, plus the conventional
/// hostname `localhost`. Anything else (DNS names, `0.0.0.0`, etc.) is
/// conservatively treated as non-loopback.
fn host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

async fn require_bearer(
    expected: Arc<String>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match provided {
        Some(header_value) if HeaderValue::from_str(header_value).ok().is_some() => {
            if header_value == expected.as_str() {
                Ok(next.run(req).await)
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn resolve_launch_config() -> anyhow::Result<LaunchConfig> {
    resolve_launch_config_from_iter(std::env::args().skip(1))
}

fn resolve_launch_config_from_iter<I>(args: I) -> anyhow::Result<LaunchConfig>
where
    I: IntoIterator<Item = String>,
{
    let mut iter = args.into_iter();
    let mut mode: Option<String> = None;
    let mut transport: Option<String> = std::env::var(TRANSPORT_ENV).ok();
    let mut http_host: Option<String> = std::env::var(HTTP_HOST_ENV).ok();
    let mut http_port: Option<String> = std::env::var(HTTP_PORT_ENV).ok();
    let mut http_token: Option<String> = std::env::var(HTTP_TOKEN_ENV).ok();
    let mut http_stateless = false;
    let mut http_json_response = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--mode" => {
                mode = Some(iter.next().ok_or_else(|| {
                    anyhow::anyhow!("Missing value for --mode. Expected 'global' or 'orchestrator'")
                })?);
            }
            "--transport" => {
                transport = Some(iter.next().ok_or_else(|| {
                    anyhow::anyhow!("Missing value for --transport. Expected 'stdio' or 'http'")
                })?);
            }
            "--http-host" => {
                http_host = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing value for --http-host"))?,
                );
            }
            "--http-port" => {
                http_port = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing value for --http-port"))?,
                );
            }
            "--http-token" => {
                http_token = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("Missing value for --http-token"))?,
                );
            }
            "--http-stateless" => {
                http_stateless = true;
            }
            "--http-json" => {
                http_json_response = true;
            }
            "-h" | "--help" => {
                println!("{}", help_text());
                std::process::exit(0);
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unknown argument '{arg}'.\n{}",
                    help_text()
                ));
            }
        }
    }

    let mode = match mode
        .as_deref()
        .unwrap_or("global")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "global" => McpLaunchMode::Global,
        "orchestrator" => McpLaunchMode::Orchestrator,
        value => {
            return Err(anyhow::anyhow!(
                "Invalid MCP mode '{value}'. Expected 'global' or 'orchestrator'"
            ));
        }
    };

    let transport_kind = transport
        .as_deref()
        .unwrap_or("stdio")
        .trim()
        .to_ascii_lowercase();

    let transport = match transport_kind.as_str() {
        "stdio" => Transport::Stdio,
        "http" | "streamable-http" => {
            let host = http_host.unwrap_or_else(|| DEFAULT_HTTP_HOST.to_string());
            let port = match http_port.as_deref() {
                Some(p) => p
                    .parse::<u16>()
                    .map_err(|e| anyhow::anyhow!("Invalid --http-port '{p}': {e}"))?,
                None => DEFAULT_HTTP_PORT,
            };
            Transport::Http(HttpConfig {
                host,
                port,
                token: http_token.filter(|t| !t.is_empty()),
                stateless: http_stateless,
                json_response: http_json_response,
            })
        }
        value => {
            return Err(anyhow::anyhow!(
                "Invalid --transport '{value}'. Expected 'stdio' or 'http'"
            ));
        }
    };

    Ok(LaunchConfig { mode, transport })
}

fn help_text() -> &'static str {
    "Usage: vibe-kanban-mcp [--mode <global|orchestrator>] [--transport <stdio|http>] \
     [--http-host <host>] [--http-port <port>] [--http-token <token>] [--http-stateless] \
     [--http-json]"
}

async fn resolve_base_url(log_prefix: &str) -> anyhow::Result<String> {
    if let Ok(url) = std::env::var("VIBE_BACKEND_URL") {
        tracing::info!(
            "[{}] Using backend URL from VIBE_BACKEND_URL: {}",
            log_prefix,
            url
        );
        return Ok(url);
    }

    let host = std::env::var(HOST_ENV)
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "127.0.0.1".to_string());

    let port = match std::env::var(PORT_ENV)
        .or_else(|_| std::env::var("BACKEND_PORT"))
        .or_else(|_| std::env::var("PORT"))
    {
        Ok(port_str) => {
            tracing::info!("[{}] Using port from environment: {}", log_prefix, port_str);
            port_str
                .parse::<u16>()
                .map_err(|error| anyhow::anyhow!("Invalid port value '{}': {}", port_str, error))?
        }
        Err(_) => {
            let port = read_port_file("vibe-kanban").await?;
            tracing::info!("[{}] Using port from port file: {}", log_prefix, port);
            port
        }
    };

    let url = format!("http://{}:{}", host, port);
    tracing::info!("[{}] Using backend URL: {}", log_prefix, url);
    Ok(url)
}

fn init_process_logging(log_prefix: &str, version: &str) {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    sentry_utils::init_once(SentrySource::Mcp);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(EnvFilter::new("debug")),
        )
        .with(sentry_layer())
        .init();

    tracing::debug!(
        "[{}] Starting Vibe Kanban MCP server version {}...",
        log_prefix,
        version
    );
}

#[cfg(test)]
mod tests {
    use super::{
        HttpConfig, LaunchConfig, McpLaunchMode, Transport, host_is_loopback,
        resolve_launch_config_from_iter,
    };

    #[test]
    fn host_is_loopback_recognises_ipv4_ipv6_and_localhost() {
        assert!(host_is_loopback("127.0.0.1"));
        assert!(host_is_loopback("127.0.0.5"));
        assert!(host_is_loopback("::1"));
        assert!(host_is_loopback("localhost"));
        assert!(host_is_loopback("LocalHost"));

        assert!(!host_is_loopback("0.0.0.0"));
        assert!(!host_is_loopback("192.168.1.1"));
        assert!(!host_is_loopback("example.com"));
    }

    #[test]
    fn defaults_to_stdio_global() {
        let config = resolve_launch_config_from_iter(std::iter::empty::<String>())
            .expect("config should parse");
        assert_eq!(
            config,
            LaunchConfig {
                mode: McpLaunchMode::Global,
                transport: Transport::Stdio,
            }
        );
    }

    #[test]
    fn orchestrator_mode_does_not_require_session_id() {
        let config = resolve_launch_config_from_iter(
            ["--mode".to_string(), "orchestrator".to_string()].into_iter(),
        )
        .expect("config should parse");

        assert_eq!(
            config,
            LaunchConfig {
                mode: McpLaunchMode::Orchestrator,
                transport: Transport::Stdio,
            }
        );
    }

    #[test]
    fn session_id_flag_is_rejected() {
        let error = resolve_launch_config_from_iter(
            [
                "--mode".to_string(),
                "orchestrator".to_string(),
                "--session-id".to_string(),
                "x".to_string(),
            ]
            .into_iter(),
        )
        .expect_err("session id flag should be rejected");

        assert!(
            error
                .to_string()
                .contains("Unknown argument '--session-id'")
        );
    }

    #[test]
    fn http_transport_uses_defaults() {
        let config = resolve_launch_config_from_iter(
            ["--transport".to_string(), "http".to_string()].into_iter(),
        )
        .expect("config should parse");

        assert_eq!(
            config,
            LaunchConfig {
                mode: McpLaunchMode::Global,
                transport: Transport::Http(HttpConfig {
                    host: "127.0.0.1".to_string(),
                    port: 4096,
                    token: None,
                    stateless: false,
                    json_response: false,
                }),
            }
        );
    }

    #[test]
    fn http_transport_accepts_overrides() {
        let config = resolve_launch_config_from_iter(
            [
                "--transport",
                "http",
                "--http-host",
                "0.0.0.0",
                "--http-port",
                "9000",
                "--http-token",
                "secret",
                "--http-stateless",
                "--http-json",
            ]
            .into_iter()
            .map(|s| s.to_string()),
        )
        .expect("config should parse");

        assert_eq!(
            config,
            LaunchConfig {
                mode: McpLaunchMode::Global,
                transport: Transport::Http(HttpConfig {
                    host: "0.0.0.0".to_string(),
                    port: 9000,
                    token: Some("secret".to_string()),
                    stateless: true,
                    json_response: true,
                }),
            }
        );
    }

    #[test]
    fn invalid_transport_is_rejected() {
        let error = resolve_launch_config_from_iter(
            ["--transport".to_string(), "carrier-pigeon".to_string()].into_iter(),
        )
        .expect_err("unknown transport should fail");
        assert!(error.to_string().contains("Invalid --transport"));
    }

    #[test]
    fn invalid_http_port_is_rejected() {
        let error = resolve_launch_config_from_iter(
            [
                "--transport".to_string(),
                "http".to_string(),
                "--http-port".to_string(),
                "not-a-port".to_string(),
            ]
            .into_iter(),
        )
        .expect_err("bad port should fail");
        assert!(error.to_string().contains("Invalid --http-port"));
    }
}
