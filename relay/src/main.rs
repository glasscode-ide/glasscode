//! glasscode-relay's process: read the shared secret from the environment, then serve until
//! SIGTERM (a container stop) or Ctrl-C.

use std::net::SocketAddr;

use glasscode_relay::{Config, switchboard, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "glasscode_relay=info".into()))
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let config = Config::from_env()?;
    let addr: SocketAddr = std::env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into()).parse()?;
    let state = switchboard::Switchboard::new(config.secret);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("glasscode-relay listening on {addr}");
    axum::serve(listener, router(state)).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

/// Containers stop with SIGTERM; a terminal with Ctrl-C.
async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("glasscode-relay shutting down");
}
