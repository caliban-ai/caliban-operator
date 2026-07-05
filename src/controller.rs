//! CalibanTask controller. #282: a no-op reconcile that initializes status to
//! Pending. The real reconcile (→ agent-sandbox Sandbox + RBAC/NetworkPolicy) is
//! caliban-ai/caliban#283.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, ResourceExt};

use crate::crd::{CalibanTask, CalibanTaskStatus, Phase};

/// Controller error.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Kubernetes API error.
    #[error("kube api: {0}")]
    Kube(#[from] kube::Error),
    /// Status serialization error.
    #[error("serialize status: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Shared reconcile context.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
}

/// The pure status decision (unit-testable; see tests).
pub(crate) fn desired_status(current: Option<&CalibanTaskStatus>) -> Option<CalibanTaskStatus> {
    match current {
        // No status yet: initialize to Pending (#282). Once any status exists —
        // Pending or progressed by #283's reconcile — leave it untouched.
        None => Some(CalibanTaskStatus {
            phase: Phase::Pending,
            ..Default::default()
        }),
        Some(_) => None,
    }
}

async fn reconcile(obj: Arc<CalibanTask>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = obj.namespace().unwrap_or_default();
    let name = obj.name_any();
    let api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);

    if let Some(status) = desired_status(obj.status.as_ref()) {
        let patch = serde_json::json!({ "status": status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, "initialized CalibanTask status to Pending");
    }
    // #282 does nothing else; requeue on a slow cadence.
    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(_obj: Arc<CalibanTask>, err: &Error, _ctx: Arc<Context>) -> Action {
    tracing::warn!(error = %err, "reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the CalibanTask controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let tasks: Api<CalibanTask> = Api::all(client.clone());
    let ctx = Arc::new(Context { client });
    Controller::new(tasks, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _action)) => tracing::debug!(?obj, "reconciled"),
                Err(e) => tracing::warn!(error = %e, "controller error"),
            }
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::crd::{CalibanTaskStatus, Phase};

    #[test]
    fn initializes_absent_status_to_pending() {
        let d = super::desired_status(None).expect("should set status");
        assert_eq!(d.phase, Phase::Pending);
    }

    #[test]
    fn leaves_running_status_untouched() {
        let running = CalibanTaskStatus {
            phase: Phase::Running,
            ..Default::default()
        };
        assert!(super::desired_status(Some(&running)).is_none());
    }

    #[test]
    fn does_not_repatch_already_pending_status() {
        let pending = CalibanTaskStatus {
            phase: Phase::Pending,
            ..Default::default()
        };
        assert!(super::desired_status(Some(&pending)).is_none());
    }

    #[test]
    fn leaves_pending_with_endpoint_untouched() {
        let pending_with_endpoint = CalibanTaskStatus {
            phase: Phase::Pending,
            caliband_endpoint: Some("10.0.0.1:9000".to_string()),
            ..Default::default()
        };
        assert!(super::desired_status(Some(&pending_with_endpoint)).is_none());
    }
}
