//! caliban-operator entrypoint.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
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
