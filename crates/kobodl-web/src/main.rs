use std::path::PathBuf;

use anyhow::Context;
use kobo::Endpoints;
use kobodl_web::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let listen = std::env::var("KOBODL_LISTEN").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
    let data_dir = PathBuf::from(std::env::var_os("KOBODL_DATA_DIR").unwrap_or_else(|| "data".into()));
    std::fs::create_dir_all(&data_dir).with_context(|| format!("can't create {}", data_dir.display()))?;

    let output_dir = std::env::var_os("KOBODL_OUTPUT_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    if let Some(output_dir) = &output_dir {
        tracing::info!("books can be saved to {}", output_dir.display());
    }

    let app = kobodl_web::app(Config {
        data_dir,
        output_dir,
        endpoints: Endpoints::default(),
    })?;

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("can't listen on {listen}"))?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
