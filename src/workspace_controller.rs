//! The `Workspace` controller: validates each `Workspace` (the operator is the
//! sole Secret reader) and writes `status.phase` / `message` /
//! `observedGeneration`. Provisions nothing. See ADR 0004.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::core::v1::Secret;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, ResourceExt};

use crate::workspace::{validate_workspace, Workspace, WorkspaceStatus, WorkspaceValidation};

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

/// Derive the new `WorkspaceStatus` from a validation result. Returns `Some`
/// only when it differs from the observed status (no-op-churn avoidance,
/// mirroring `controller::derive_status`).
pub(crate) fn derive_workspace_status(
    ws: &Workspace,
    v: WorkspaceValidation,
) -> Option<WorkspaceStatus> {
    let mut next = ws.status.clone().unwrap_or_default();
    next.phase = v.phase;
    next.message = v.message;
    next.observed_generation = ws.metadata.generation;
    match &ws.status {
        Some(cur)
            if cur.phase == next.phase
                && cur.message == next.message
                && cur.observed_generation == next.observed_generation =>
        {
            None
        }
        _ => Some(next),
    }
}

async fn reconcile(ws: Arc<Workspace>, ctx: Arc<Client>) -> Result<Action, Error> {
    let ns = ws.namespace().unwrap_or_default();
    let name = ws.name_any();
    let secrets: Api<Secret> = Api::namespaced((*ctx).clone(), &ns);

    // Resolve which (secretName, key) pairs actually exist, once, up front, so
    // validate_workspace stays pure.
    let mut present: std::collections::BTreeSet<(String, String)> = Default::default();
    for p in &ws.spec.providers {
        if let Some(c) = &p.credentials_ref {
            if let Some(sec) = secrets.get_opt(&c.secret_name).await? {
                let has = sec.data.as_ref().is_some_and(|d| d.contains_key(&c.key))
                    || sec
                        .string_data
                        .as_ref()
                        .is_some_and(|d| d.contains_key(&c.key));
                if has {
                    present.insert((c.secret_name.clone(), c.key.clone()));
                }
            }
        }
    }
    let validation = validate_workspace(&ws.spec, |s, k| {
        present.contains(&(s.to_string(), k.to_string()))
    });

    if let Some(status) = derive_workspace_status(&ws, validation) {
        let api: Api<Workspace> = Api::namespaced((*ctx).clone(), &ns);
        let phase = status.phase;
        let patch = serde_json::json!({ "status": status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, ?phase, "patched Workspace status");
    }
    Ok(Action::requeue(Duration::from_secs(300)))
}

fn error_policy(_ws: Arc<Workspace>, err: &Error, _ctx: Arc<Client>) -> Action {
    tracing::warn!(error = %err, "workspace reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the Workspace controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let workspaces: Api<Workspace> = Api::all(client.clone());
    let ctx = Arc::new(client);
    Controller::new(workspaces, Config::default())
        .shutdown_on_signal()
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => tracing::debug!(?obj, "workspace reconciled"),
                Err(e) => tracing::warn!(error = %e, "workspace controller error"),
            }
        })
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::Source;
    use crate::workspace::{Provider, WorkspacePhase, WorkspaceSpec};

    fn workspace(gen: i64) -> Workspace {
        let mut ws = Workspace::new(
            "team-a-ws",
            WorkspaceSpec {
                display_name: "Team A".into(),
                sources: vec![Source {
                    name: "caliban".into(),
                    repo: "git@x:caliban".into(),
                    r#ref: "main".into(),
                    path: "/work/caliban".into(),
                }],
                providers: vec![Provider {
                    name: "workers".into(),
                    kind: "ollama".into(),
                    base_url: None,
                    model: None,
                    credentials_ref: None,
                }],
                default_provider: None,
                env: vec![],
                isolation: None,
            },
        );
        ws.metadata.namespace = Some("team-a".into());
        ws.metadata.generation = Some(gen);
        ws
    }

    #[test]
    fn ready_status_is_derived_and_records_generation() {
        let ws = workspace(3);
        let v = validate_workspace(&ws.spec, |_, _| true);
        let s = derive_workspace_status(&ws, v).unwrap();
        assert_eq!(s.phase, WorkspacePhase::Ready);
        assert_eq!(s.observed_generation, Some(3));
        assert!(s.message.is_none());
    }

    #[test]
    fn unchanged_status_is_noop() {
        let mut ws = workspace(3);
        let v = validate_workspace(&ws.spec, |_, _| true);
        ws.status = derive_workspace_status(&ws, v);
        let v2 = validate_workspace(&ws.spec, |_, _| true);
        assert!(derive_workspace_status(&ws, v2).is_none());
    }
}
