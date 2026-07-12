//! CalibanTask controller (#283): reconciles a `CalibanTask` into its child
//! objects — a token-less ServiceAccount, a default-deny NetworkPolicy, and the
//! backing agent-sandbox Sandbox — via server-side apply, then derives and
//! patches the task's status from the Sandbox's observed state.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::networking::v1::NetworkPolicy;
use kube::api::{Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, Resource, ResourceExt};

use crate::config::{sandbox_name, Settings};
use crate::crd::{CalibanTask, CalibanTaskStatus, Condition, NamedRef, Phase};
use crate::resources::plan;
use crate::sandbox::Sandbox;
use crate::workspace::resolve_workspace;
use crate::workspace::Workspace;

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
    /// Operator settings.
    pub settings: Settings,
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
    next.sandbox_ref = sandbox.map(|_| NamedRef {
        name: sandbox_name(t),
    });
    // `derive_status` is the single owner of the `Ready` condition and is only
    // ever reached after the workspace has resolved (the fail-fast branch in
    // `reconcile` returns early, before this is called). So any
    // `WorkspaceUnresolved` condition carried in the stale in-memory
    // `t.status` (cloned above) is by definition stale and must not survive:
    // derive it fresh from the phase instead of leaving whatever was there.
    next.conditions = match phase {
        Phase::Running => vec![Condition {
            type_: "Ready".into(),
            status: "True".into(),
            reason: Some("Running".into()),
            message: None,
        }],
        _ => Vec::new(),
    };
    match &t.status {
        Some(cur) if status_eq(cur, &next) => None,
        _ => Some(next),
    }
}

fn status_eq(a: &CalibanTaskStatus, b: &CalibanTaskStatus) -> bool {
    a.phase == b.phase
        && a.caliband_endpoint == b.caliband_endpoint
        && a.sandbox_ref.as_ref().map(|r| &r.name) == b.sandbox_ref.as_ref().map(|r| &r.name)
        && a.conditions == b.conditions
}

/// Server-side apply `obj` under `name`, force-owned by the operator's field
/// manager. Used for the operator's own children (SA, NetworkPolicy, Sandbox).
async fn apply<K>(api: &Api<K>, name: &str, obj: &K) -> Result<(), Error>
where
    K: Clone + std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned + Resource,
    K::DynamicType: Default,
{
    let pp = PatchParams::apply("caliban-operator").force();
    api.patch(name, &pp, &Patch::Apply(obj)).await?;
    Ok(())
}

/// Admission decision for a `CalibanTask` against its referenced `Workspace`,
/// evaluated once before pinning. Pure — cluster I/O (the `Workspace` fetch and
/// the status patch) is the caller's job.
pub(crate) enum Admission {
    /// The workspace is `Ready` and resolves cleanly: pin this config.
    Pin(Box<crate::workspace::ResolvedWorkspace>),
    /// The workspace exists but isn't `Ready` yet (still `Pending`): don't pin,
    /// requeue and re-check once its own controller has validated it.
    Requeue,
    /// Terminal task-config error — missing workspace, a `Failed` workspace, or
    /// an unresolvable `providerRef`. The string is the human-readable reason.
    Fail(String),
}

/// Decide whether a `CalibanTask` may pin its `workspaceRef`/`providerRef`.
/// Gates on the referenced `Workspace`'s readiness so a task never launches a
/// doomed pod against a `Failed` workspace (e.g. a missing credential Secret)
/// nor pins against a not-yet-validated one.
pub(crate) fn admit(
    ws: Option<&Workspace>,
    provider_ref: Option<&str>,
    workspace_ref_name: &str,
) -> Admission {
    let Some(w) = ws else {
        return Admission::Fail(format!("Workspace not found: '{workspace_ref_name}'"));
    };
    let phase = w.status.as_ref().map(|s| s.phase).unwrap_or_default();
    match phase {
        crate::workspace::WorkspacePhase::Failed => {
            let detail = w
                .status
                .as_ref()
                .and_then(|s| s.message.clone())
                .unwrap_or_else(|| "validation failed".to_string());
            Admission::Fail(format!(
                "workspace '{workspace_ref_name}' not ready: {detail}"
            ))
        }
        crate::workspace::WorkspacePhase::Ready => match resolve_workspace(&w.spec, provider_ref) {
            Ok(rw) => Admission::Pin(Box::new(rw)),
            Err(reason) => Admission::Fail(reason),
        },
        // Pending: the Workspace controller hasn't validated it yet. Wait.
        crate::workspace::WorkspacePhase::Pending => Admission::Requeue,
    }
}

async fn reconcile(obj: Arc<CalibanTask>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = obj.namespace().unwrap_or_default();
    let name = obj.name_any();
    let s = &ctx.settings;

    // Pin once: a running task keeps the config it was admitted with. Later
    // edits to the referenced `Workspace` don't re-pin (or disturb) a running task.
    let resolved = match obj
        .status
        .as_ref()
        .and_then(|st| st.resolved_workspace.clone())
    {
        Some(rw) => rw,
        None => {
            let ws_api: Api<Workspace> = Api::namespaced(ctx.client.clone(), &ns);
            let ws = ws_api.get_opt(&obj.spec.workspace_ref.name).await?;
            // Gate on the referenced Workspace's readiness before pinning, so a
            // task never launches a doomed pod against a Failed workspace nor
            // pins a not-yet-validated one. `admit` also distinguishes "no such
            // Workspace" and a dangling `providerRef` so the specific reason
            // reaches the condition's human-readable `message` (reason stays the
            // fixed `WorkspaceUnresolved`).
            match admit(
                ws.as_ref(),
                obj.spec.provider_ref.as_deref(),
                &obj.spec.workspace_ref.name,
            ) {
                Admission::Pin(rw) => {
                    let rw = *rw;
                    // Persist the pin immediately so it's stable for the run.
                    // Any stale `WorkspaceUnresolved` condition left by a prior
                    // fail-fast reconcile is cleared once `derive_status` runs
                    // later this same reconcile (it owns `conditions` and
                    // derives them fresh from the phase), so it need not be
                    // touched here.
                    let patch = serde_json::json!({
                        "status": { "resolvedWorkspace": rw }
                    });
                    let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                    task_api
                        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await?;
                    rw
                }
                Admission::Requeue => {
                    // Workspace exists but isn't Ready yet; don't pin. Its own
                    // controller will validate it shortly — re-check soon.
                    tracing::info!(%ns, %name, "workspace not ready; deferring pin");
                    return Ok(Action::requeue(Duration::from_secs(10)));
                }
                Admission::Fail(reason) => {
                    let patch = serde_json::json!({
                        "status": { "phase": Phase::Failed,
                            "conditions": [{ "type": "Ready", "status": "False",
                                "reason": "WorkspaceUnresolved",
                                "message": reason }] }
                    });
                    let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                    task_api
                        .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
                        .await?;
                    tracing::warn!(%ns, %name, "workspace unresolved; task Failed");
                    return Ok(Action::requeue(Duration::from_secs(30)));
                }
            }
        }
    };
    let p = plan(&obj, &resolved, s);

    // Apply children (idempotent SSA; owner refs cascade-delete).
    let sa_api: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&sa_api, &p.service_account.name_any(), &p.service_account).await?;
    let np_api: Api<NetworkPolicy> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&np_api, &p.network_policy.name_any(), &p.network_policy).await?;
    let sb_api: Api<Sandbox> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&sb_api, &p.sandbox.name_any(), &p.sandbox).await?;

    // Read the Sandbox back for its status, then derive + patch CalibanTask status.
    let sandbox = sb_api.get_opt(&sandbox_name(&obj)).await?;
    if let Some(status) = derive_status(&obj, sandbox.as_ref(), s) {
        let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
        let phase = status.phase;
        let patch = serde_json::json!({ "status": status });
        task_api
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, ?phase, "patched CalibanTask status");
    }
    Ok(Action::requeue(Duration::from_secs(30)))
}

fn error_policy(_obj: Arc<CalibanTask>, err: &Error, _ctx: Arc<Context>) -> Action {
    tracing::warn!(error = %err, "reconcile error");
    Action::requeue(Duration::from_secs(30))
}

/// Run the CalibanTask controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let tasks: Api<CalibanTask> = Api::all(client.clone());
    let ctx = Arc::new(Context {
        client,
        settings: Settings::from_env(),
    });
    Controller::new(tasks, Config::default())
        // Drain in-flight reconciles on SIGTERM/SIGINT (pod termination, Ctrl+C)
        // instead of hard-killing mid-reconcile.
        .shutdown_on_signal()
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
    use crate::crd::{CalibanTaskSpec, Source, TaskSpec, WorkspaceRef};
    use crate::sandbox::{SandboxSpec, SandboxStatus};
    use crate::workspace::{Provider, Workspace, WorkspacePhase, WorkspaceSpec, WorkspaceStatus};
    use k8s_openapi::api::core::v1::PodTemplateSpec;

    fn workspace(phase: WorkspacePhase, message: Option<&str>) -> Workspace {
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
        ws.status = Some(WorkspaceStatus {
            phase,
            message: message.map(String::from),
            ..Default::default()
        });
        ws
    }

    #[test]
    fn admit_pins_a_ready_workspace() {
        let ws = workspace(WorkspacePhase::Ready, None);
        match admit(Some(&ws), None, "team-a-ws") {
            Admission::Pin(rw) => assert_eq!(rw.provider.name, "workers"),
            other => panic!("expected Pin, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn admit_requeues_a_pending_workspace() {
        let ws = workspace(WorkspacePhase::Pending, None);
        assert!(matches!(
            admit(Some(&ws), None, "team-a-ws"),
            Admission::Requeue
        ));
    }

    #[test]
    fn admit_fails_a_failed_workspace_and_surfaces_its_message() {
        let ws = workspace(
            WorkspacePhase::Failed,
            Some("provider 'workers': secret 'k' key 'v' not found"),
        );
        match admit(Some(&ws), None, "team-a-ws") {
            Admission::Fail(msg) => assert_eq!(
                msg,
                "workspace 'team-a-ws' not ready: provider 'workers': secret 'k' key 'v' not found"
            ),
            _ => panic!("expected Fail"),
        }
    }

    #[test]
    fn admit_fails_a_missing_workspace() {
        match admit(None, None, "gone-ws") {
            Admission::Fail(msg) => assert_eq!(msg, "Workspace not found: 'gone-ws'"),
            _ => panic!("expected Fail"),
        }
    }

    #[test]
    fn admit_fails_a_ready_workspace_with_a_dangling_provider_ref() {
        let ws = workspace(WorkspacePhase::Ready, None);
        match admit(Some(&ws), Some("nope"), "team-a-ws") {
            Admission::Fail(msg) => {
                assert_eq!(msg, "providerRef 'nope' names no provider in the workspace")
            }
            _ => panic!("expected Fail"),
        }
    }

    fn task_without_status() -> CalibanTask {
        let mut t = CalibanTask::new(
            "refactor-auth",
            CalibanTaskSpec {
                workspace_ref: WorkspaceRef {
                    name: "team-a-ws".into(),
                },
                provider_ref: None,
                task: TaskSpec {
                    prompt: "hi".into(),
                    agent_type: None,
                },
                model: None,
                state: None,
                isolation: None,
                resources: None,
                lifecycle: None,
                tools: None,
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
        assert!(d.conditions.is_empty());
    }

    #[test]
    fn sandbox_without_fqdn_is_provisioning() {
        let t = task_without_status();
        let sb = sandbox_with_fqdn(None);
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Provisioning);
        assert_eq!(d.sandbox_ref.unwrap().name, "refactor-auth-sbx");
        assert!(d.conditions.is_empty());
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
        // Running phase must carry a derived Ready=True condition.
        assert_eq!(d.conditions.len(), 1);
        assert_eq!(d.conditions[0].type_, "Ready");
        assert_eq!(d.conditions[0].status, "True");
    }

    #[test]
    fn unchanged_status_is_noop() {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        // First derivation, then apply it as the observed status.
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
    }

    #[test]
    fn stale_workspace_unresolved_condition_does_not_survive_into_running() {
        // Regression guard: a task carrying a stale `Ready=False /
        // WorkspaceUnresolved` condition (left over from an earlier fail-fast
        // reconcile) must NOT keep that condition once the workspace resolves
        // and the Sandbox comes up Running. `derive_status` owns
        // `conditions` and must derive them fresh from the phase it computes,
        // not carry forward whatever was in the stale in-memory status.
        let mut t = task_without_status();
        t.status = Some(CalibanTaskStatus {
            phase: Phase::Failed,
            conditions: vec![Condition {
                type_: "Ready".into(),
                status: "False".into(),
                reason: Some("WorkspaceUnresolved".into()),
                message: Some("workspaceRef 'team-a-ws' / providerRef unresolved".into()),
            }],
            ..Default::default()
        });
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Running);
        assert!(
            !d.conditions
                .iter()
                .any(|c| c.reason.as_deref() == Some("WorkspaceUnresolved")),
            "stale WorkspaceUnresolved condition survived into Running: {:?}",
            d.conditions
        );
        assert_eq!(d.conditions.len(), 1);
        assert_eq!(d.conditions[0].status, "True");
    }

    #[test]
    fn sandbox_disappearing_clears_ref_and_returns_to_pending() {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        // Observed status reflects a running sandbox...
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        assert!(t.status.as_ref().unwrap().sandbox_ref.is_some());
        // ...then the sandbox is gone: status must return to Pending with no ref.
        let d = super::derive_status(&t, None, &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Pending);
        assert!(d.sandbox_ref.is_none());
        assert!(d.caliband_endpoint.is_none());
    }

    #[test]
    fn cleared_endpoint_and_ref_serialize_as_null_for_merge_delete() {
        // A status with a running endpoint, then cleared back to Pending.
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        let cleared = super::derive_status(&t, None, &Settings::default()).unwrap();
        let v = serde_json::to_value(&cleared).unwrap();
        // Merge-patch needs explicit null (not absent) to delete the stale values.
        assert!(v.get("calibandEndpoint").is_some_and(|x| x.is_null()));
        assert!(v.get("sandboxRef").is_some_and(|x| x.is_null()));
        // Regression guard: a Running->Pending transition must serialize
        // `conditions` as an explicit empty array, not omit the key. Under
        // JSON Merge Patch an omitted key is left unchanged server-side, so
        // if `conditions` were skipped here the stale `Ready=True` condition
        // would never be cleared and every subsequent reconcile would keep
        // re-patching (endless churn).
        assert_eq!(v.get("conditions"), Some(&serde_json::json!([])));
    }
}
