//! caliban-operator entrypoint.

use caliban_operator::config::{leader_identity, Settings};
use kube_lease_manager::LeaseManagerBuilder;
use tokio::sync::watch::Receiver;

/// Where the in-cluster namespace is published to a pod.
const NAMESPACE_FILE: &str = "/var/run/secrets/kubernetes.io/serviceaccount/namespace";

/// Run both controllers until one of them stops.
async fn run_controllers(client: kube::Client) -> anyhow::Result<()> {
    tokio::try_join!(
        caliban_operator::controller::run(client.clone()),
        caliban_operator::workspace_controller::run(client),
    )?;
    Ok(())
}

/// The namespace holding the leader lease: the configured one, else the
/// namespace this pod runs in.
fn lease_namespace(s: &Settings) -> anyhow::Result<String> {
    if let Some(ns) = &s.lease_namespace {
        return Ok(ns.clone());
    }
    let ns = std::fs::read_to_string(NAMESPACE_FILE).map_err(|e| {
        anyhow::anyhow!(
            "cannot determine the lease namespace: {NAMESPACE_FILE} is unreadable ({e}). \
             Set CALIBAN_LEASE_NAMESPACE, or CALIBAN_LEADER_ELECTION=false to run without a lease."
        )
    })?;
    let ns = ns.trim().to_string();
    if ns.is_empty() {
        anyhow::bail!("cannot determine the lease namespace: {NAMESPACE_FILE} is empty");
    }
    Ok(ns)
}

/// Resolve once the receiver reports the wanted leadership state.
async fn await_leadership(leader: &mut Receiver<bool>, wanted: bool) -> anyhow::Result<()> {
    loop {
        if *leader.borrow_and_update() == wanted {
            return Ok(());
        }
        // The sender lives for as long as the watch task; if it is gone the
        // lease can no longer be renewed, so there is nothing to lead.
        leader
            .changed()
            .await
            .map_err(|e| anyhow::anyhow!("the leader-election watch stopped: {e}"))?;
    }
}

/// Run the controllers only while this replica holds the leader lease (#48).
///
/// Losing the lease stops the controllers: their futures are dropped, which
/// ends their watches, and the process returns to standby rather than exiting
/// — so a brief apiserver blip costs a restart's worth of churn instead of a
/// crash loop. A replica that never wins simply waits.
async fn run_elected(client: kube::Client, s: &Settings) -> anyhow::Result<()> {
    let namespace = lease_namespace(s)?;
    let identity = leader_identity(std::env::var("POD_NAME").ok().as_deref());
    tracing::info!(
        lease = %s.lease_name,
        namespace = %namespace,
        identity = %identity,
        "contending for the leader lease"
    );
    let manager = LeaseManagerBuilder::new(client.clone(), &s.lease_name)
        .with_namespace(namespace)
        .with_identity(identity.clone())
        .with_duration(s.lease_duration_seconds)
        .with_grace(s.lease_grace_seconds)
        .with_field_manager("caliban-operator")
        .build()
        .await?;
    // Holding the receiver keeps the lease renewing; dropping every receiver
    // releases it, so a clean shutdown hands over instead of waiting out the
    // duration.
    let (mut leader, _watch) = manager.watch().await;
    loop {
        await_leadership(&mut leader, true).await?;
        tracing::info!(identity = %identity, "acquired the leader lease; starting controllers");
        tokio::select! {
            // The controllers returning is terminal either way: propagate it.
            r = run_controllers(client.clone()) => return r,
            r = await_leadership(&mut leader, false) => {
                r?;
                tracing::warn!(
                    identity = %identity,
                    "lost the leader lease; stopping controllers and standing by"
                );
            }
        }
    }
}

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
    let settings = Settings::from_env();
    if let Err(e) = settings.validate() {
        anyhow::bail!("invalid configuration: {e}");
    }
    let client = kube::Client::try_default().await?;
    tracing::info!("connected to the Kubernetes API");
    if settings.leader_election {
        run_elected(client, &settings).await
    } else {
        // Single-replica installs that manage exclusivity themselves: the
        // behaviour before #48, unchanged.
        tracing::info!("leader election disabled; running the controllers directly");
        run_controllers(client).await
    }
}
