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

use crate::config::Settings;
use crate::workspace::{
    validate_workspace, Workspace, WorkspacePhase, WorkspaceStatus, WorkspaceValidation,
};

/// Shared reconcile context: the API client plus the settings validation needs.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
    /// Workspace root the sandbox mounts the PVC at; sources must sit under it (#46).
    pub workspace_root: String,
}

pub use crate::error::Error;

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

/// How soon to re-check a workspace (#47). The controller deliberately does not
/// watch Secrets — that would need cluster-wide `list`/`watch` on every Secret
/// and cache their data in the operator — so recovery from a missing credential
/// rides this requeue. A workspace that isn't `Ready` is re-checked quickly, so
/// creating the Secret unblocks it (and the tasks waiting on it) within
/// seconds; a `Ready` one keeps a slow safety-net interval.
pub(crate) fn requeue_after(phase: WorkspacePhase) -> Duration {
    match phase {
        WorkspacePhase::Ready => Duration::from_secs(300),
        WorkspacePhase::Failed | WorkspacePhase::Pending => Duration::from_secs(15),
    }
}

/// The distinct Secret names a workspace's providers reference, in stable order
/// (#55). Providers often share one Secret under different keys; reconcile
/// fetches each Secret once rather than once per provider.
pub(crate) fn credential_secret_names(
    spec: &crate::workspace::WorkspaceSpec,
) -> std::collections::BTreeSet<&str> {
    spec.providers
        .iter()
        .filter_map(|p| p.credentials_ref.as_ref())
        .map(|c| c.secret_name.as_str())
        .collect()
}

async fn reconcile(ws: Arc<Workspace>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = ws.namespace().unwrap_or_default();
    let name = ws.name_any();
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), &ns);

    // Fetch each referenced Secret once, then resolve which (secretName, key)
    // pairs actually exist up front, so validate_workspace stays pure.
    let mut fetched: std::collections::BTreeMap<&str, Secret> = Default::default();
    for secret_name in credential_secret_names(&ws.spec) {
        if let Some(sec) = secrets.get_opt(secret_name).await? {
            fetched.insert(secret_name, sec);
        }
    }
    let mut present: std::collections::BTreeSet<(String, String)> = Default::default();
    for c in ws
        .spec
        .providers
        .iter()
        .filter_map(|p| p.credentials_ref.as_ref())
    {
        if let Some(sec) = fetched.get(c.secret_name.as_str()) {
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
    let validation = validate_workspace(&ws.spec, &ctx.workspace_root, |s, k| {
        present.contains(&(s.to_string(), k.to_string()))
    });

    let phase = validation.phase;
    if let Some(status) = derive_workspace_status(&ws, validation) {
        let api: Api<Workspace> = Api::namespaced(ctx.client.clone(), &ns);
        let patch = serde_json::json!({ "status": status });
        api.patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, ?phase, "patched Workspace status");
    }
    Ok(Action::requeue(requeue_after(phase)))
}

fn error_policy(_ws: Arc<Workspace>, err: &Error, _ctx: Arc<Context>) -> Action {
    tracing::warn!(error = %err, "workspace reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the Workspace controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let workspaces: Api<Workspace> = Api::all(client.clone());
    let ctx = Arc::new(Context {
        client,
        workspace_root: Settings::from_env().workspace_root,
    });
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
    use crate::workspace::Source;
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
                    kind: "openai".into(),
                    base_url: None,
                    model: None,
                    credentials_ref: None,
                }],
                default_provider: None,
                env: vec![],
                isolation: None,
                egress: None,
            },
        );
        ws.metadata.namespace = Some("team-a".into());
        ws.metadata.generation = Some(gen);
        ws
    }

    #[test]
    fn ready_status_is_derived_and_records_generation() {
        let ws = workspace(3);
        let v = validate_workspace(&ws.spec, "/work", |_, _| true);
        let s = derive_workspace_status(&ws, v).unwrap();
        assert_eq!(s.phase, WorkspacePhase::Ready);
        assert_eq!(s.observed_generation, Some(3));
        assert!(s.message.is_none());
    }

    #[test]
    fn unchanged_status_is_noop() {
        let mut ws = workspace(3);
        let v = validate_workspace(&ws.spec, "/work", |_, _| true);
        ws.status = derive_workspace_status(&ws, v);
        let v2 = validate_workspace(&ws.spec, "/work", |_, _| true);
        assert!(derive_workspace_status(&ws, v2).is_none());
    }

    /// #47: the controller does not watch Secrets — that would need
    /// cluster-wide `list`/`watch` on every Secret and cache their data in the
    /// operator. Recovery from a missing credential therefore rides the
    /// requeue, so a workspace that isn't Ready is re-checked quickly: creating
    /// the Secret unblocks it within seconds, not five minutes. A Ready
    /// workspace keeps the slow safety-net interval.
    #[test]
    fn a_workspace_that_is_not_ready_requeues_quickly() {
        assert_eq!(
            requeue_after(WorkspacePhase::Failed),
            Duration::from_secs(15)
        );
        assert_eq!(
            requeue_after(WorkspacePhase::Pending),
            Duration::from_secs(15)
        );
        assert_eq!(
            requeue_after(WorkspacePhase::Ready),
            Duration::from_secs(300)
        );
    }

    /// #55: providers commonly share one credential Secret under different
    /// keys. Reconcile must fetch each distinct Secret once, not once per
    /// provider.
    #[test]
    fn credential_secret_names_are_deduplicated() {
        use crate::workspace::CredentialsRef;
        let with_cred = |name: &str, secret: &str, key: &str| Provider {
            name: name.into(),
            kind: "anthropic".into(),
            base_url: None,
            model: None,
            credentials_ref: Some(CredentialsRef {
                secret_name: secret.into(),
                key: key.into(),
            }),
        };
        let mut ws = workspace(1);
        ws.spec.providers = vec![
            with_cred("planner", "llm-keys", "anthropic"),
            with_cred("workers", "llm-keys", "openai"),
            with_cred("review", "other", "k"),
            Provider {
                name: "local".into(),
                kind: "openai".into(),
                base_url: None,
                model: None,
                credentials_ref: None,
            },
        ];
        let names: Vec<&str> = super::credential_secret_names(&ws.spec)
            .into_iter()
            .collect();
        assert_eq!(names, vec!["llm-keys", "other"]);
    }
}
