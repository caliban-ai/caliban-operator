//! caliban-operator entrypoint.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        // Default to `info` when `RUST_LOG` is unset (#59). `from_default_env`
        // alone yields a filter that drops everything, so running the binary
        // directly looked like a hang.
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    tracing::info!("caliban-operator starting");
    let client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    tokio::try_join!(
        caliban_operator::controller::run(client.clone()),
        caliban_operator::workspace_controller::run(client),
    )?;
    Ok(())
}
