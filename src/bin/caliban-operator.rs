//! caliban-operator entrypoint.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    tracing::info!("caliban-operator starting");
    let _client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    // Controller wiring lands in Task 4.
    Ok(())
}
