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

use crate::config::{sandbox_name, Settings};
use crate::crd::{CalibanTask, CalibanTaskStatus, NamedRef, Phase};
use crate::sandbox::Sandbox;

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

/// Derive the CalibanTask status from the task + its backing Sandbox. Returns
/// `Some(new_status)` only when it differs from the observed status (no-op churn
/// avoidance), else `None`. See ADR 0002 §6.
pub(crate) fn derive_status(
    t: &CalibanTask,
    sandbox: Option<&Sandbox>,
    s: &Settings,
) -> Option<CalibanTaskStatus> {
    let fqdn = sandbox
        .and_then(|sb| sb.status.as_ref())
        .and_then(|st| st.service_fqdn.clone());
    let (phase, endpoint) = match (sandbox, fqdn) {
        (None, _) => (Phase::Pending, None),
        (Some(_), None) => (Phase::Provisioning, None),
        (Some(_), Some(f)) => (Phase::Running, Some(format!("{}:{}", f, s.caliband_port))),
    };
    let mut next = t.status.clone().unwrap_or_default();
    next.phase = phase;
    next.caliband_endpoint = endpoint;
    if sandbox.is_some() {
        next.sandbox_ref = Some(NamedRef {
            name: sandbox_name(t),
        });
    }
    match &t.status {
        Some(cur) if status_eq(cur, &next) => None,
        _ => Some(next),
    }
}

fn status_eq(a: &CalibanTaskStatus, b: &CalibanTaskStatus) -> bool {
    a.phase == b.phase
        && a.caliband_endpoint == b.caliband_endpoint
        && a.sandbox_ref.as_ref().map(|r| &r.name) == b.sandbox_ref.as_ref().map(|r| &r.name)
}

async fn reconcile(obj: Arc<CalibanTask>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = obj.namespace().unwrap_or_default();
    let name = obj.name_any();
    let api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);

    if let Some(status) = derive_status(&obj, None, &Settings::from_env()) {
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
    use super::*;
    use crate::crd::{CalibanTaskSpec, Source, TaskSpec, Workspace};
    use crate::sandbox::{SandboxSpec, SandboxStatus};
    use k8s_openapi::api::core::v1::PodTemplateSpec;

    fn task_without_status() -> CalibanTask {
        let mut t = CalibanTask::new(
            "refactor-auth",
            CalibanTaskSpec {
                workspace: Workspace {
                    sources: vec![Source {
                        name: "caliban".into(),
                        repo: "git@x:caliban".into(),
                        r#ref: "main".into(),
                        path: "/work/caliban".into(),
                    }],
                    services: vec![],
                },
                task: TaskSpec {
                    prompt: "hi".into(),
                    agent_type: None,
                },
                model: None,
                state: None,
                isolation: None,
                resources: None,
                lifecycle: None,
            },
        );
        t.metadata.namespace = Some("team-a".into());
        t.metadata.uid = Some("uid-123".into());
        t.status = None;
        t
    }

    fn sandbox_with_fqdn(fqdn: Option<&str>) -> Sandbox {
        let mut sb = Sandbox::new(
            "refactor-auth-sbx",
            SandboxSpec {
                pod_template: PodTemplateSpec::default(),
                service: Some(true),
                operating_mode: Some("Running".to_string()),
                volume_claim_templates: None,
            },
        );
        sb.metadata.namespace = Some("team-a".into());
        sb.status = Some(SandboxStatus {
            service_fqdn: fqdn.map(|f| f.to_string()),
        });
        sb
    }

    #[test]
    fn no_sandbox_yields_pending() {
        let t = task_without_status();
        let d = super::derive_status(&t, None, &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Pending);
        assert!(d.caliband_endpoint.is_none());
    }

    #[test]
    fn sandbox_without_fqdn_is_provisioning() {
        let t = task_without_status();
        let sb = sandbox_with_fqdn(None);
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Provisioning);
        assert_eq!(d.sandbox_ref.unwrap().name, "refactor-auth-sbx");
    }

    #[test]
    fn sandbox_with_fqdn_is_running_with_endpoint() {
        let t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Running);
        assert_eq!(
            d.caliband_endpoint.as_deref(),
            Some("refactor-auth-sbx.team-a.svc:8443")
        );
    }

    #[test]
    fn unchanged_status_is_noop() {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        // First derivation, then apply it as the observed status.
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
    }
}
