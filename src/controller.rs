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
use kube::runtime::events::{Recorder, Reporter};
use kube::runtime::watcher::Config;
use kube::runtime::Controller;
use kube::{Api, Client, Resource, ResourceExt};

use crate::config::{sandbox_name, task_name_problem, Settings};
use crate::crd::{CalibanTask, CalibanTaskStatus, Condition, NamedRef, Phase};
use crate::events;
use crate::resources::plan;
use crate::sandbox::Sandbox;
use crate::workspace::resolve_workspace;
use crate::workspace::Workspace;

pub use crate::error::Error;

/// Shared reconcile context.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
    /// Operator settings.
    pub settings: Settings,
    /// Publishes Kubernetes Events on the tasks it reconciles (#56).
    pub recorder: Recorder,
}

/// Derive the CalibanTask status from the task + its backing Sandbox. Returns
/// `Some(new_status)` only when it differs from the observed status (no-op churn
/// avoidance), else `None`. See ADR 0002 §6.
pub(crate) fn derive_status(
    t: &CalibanTask,
    sandbox: Option<&Sandbox>,
    s: &Settings,
) -> Option<CalibanTaskStatus> {
    let sb_status = sandbox.and_then(|sb| sb.status.as_ref());
    let fqdn = sb_status.and_then(|st| st.service_fqdn.clone());
    // #45: `serviceFQDN` appears as soon as the Service object exists — before
    // caliband has bound its port. agent-sandbox's own `Ready` condition is True
    // only once the pod passes its readiness probe (on the control port) and
    // the Service is ready, so that, not the FQDN, is what makes a task Running.
    // No condition yet is not evidence of readiness.
    let sb_ready = sb_status.and_then(|st| st.conditions.iter().find(|c| c.type_ == "Ready"));
    let ready = sb_ready.is_some_and(|c| c.status == "True");
    let (infrastructure_phase, endpoint) = match (sandbox, fqdn) {
        (None, _) => (Phase::Pending, None),
        (Some(_), Some(f)) if ready => (Phase::Running, Some(format!("{}:{}", f, s.caliband_port))),
        (Some(_), _) => (Phase::Provisioning, None),
    };
    // #37: infrastructure cannot tell "agent running" from "agent finished, pod
    // still up" — only caliband's agent list knows, and the operator never dials
    // it (ADR 0005). prospero reports that as `AgentsSettled` (prospero#228), and
    // a settled task's phase is terminal regardless of the Sandbox.
    let settled = t
        .status
        .as_ref()
        .and_then(|st| st.conditions.iter().find(|c| c.type_ == "AgentsSettled"))
        .filter(|c| c.status == "True");
    let phase = match settled.and_then(|c| c.reason.as_deref()) {
        Some("Succeeded") => Phase::Completed,
        Some("Failed") => Phase::Failed,
        // `AgentsActive` (e.g. an idle interactive task) and any reason outside
        // prospero's contract are not terminal.
        _ => infrastructure_phase,
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
        // A Sandbox that has *reported* not-ready: say so, carrying
        // agent-sandbox's human-readable detail (e.g. "Pod is Running but not
        // Ready") so `kubectl describe` shows why the task isn't up yet.
        Phase::Provisioning => sb_ready
            .filter(|c| c.status != "True")
            .map(|c| {
                vec![Condition {
                    type_: "Ready".into(),
                    status: "False".into(),
                    reason: Some("SandboxNotReady".into()),
                    message: c.message.clone(),
                }]
            })
            .unwrap_or_default(),
        // Terminal phases (#37): the agents are done, so the task is not Ready.
        Phase::Completed => vec![Condition {
            type_: "Ready".into(),
            status: "False".into(),
            reason: Some("Completed".into()),
            message: None,
        }],
        Phase::Failed => vec![Condition {
            type_: "Ready".into(),
            status: "False".into(),
            reason: Some("AgentsFailed".into()),
            // prospero's detail, when it gives one, reaches `kubectl describe`.
            // Its `AgentsSettled` sets no message today (prospero#228), so fall
            // back rather than show an empty reason.
            message: settled
                .and_then(|c| c.message.clone())
                .or_else(|| Some("one or more agents failed or crashed".to_string())),
        }],
        _ => Vec::new(),
    };
    match &t.status {
        Some(cur) if status_eq(cur, &next) => None,
        _ => Some(next),
    }
}

/// The server-side apply body for a `CalibanTask`'s status (#64, ADR 0005): a
/// partial object that names its type and carries the operator's status fields.
pub(crate) fn status_apply_body(status: &CalibanTaskStatus) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": CalibanTask::api_version(&()),
        "kind": CalibanTask::kind(&()),
        "status": status,
    })
}

/// Condition types the operator owns on `CalibanTask` status. Any other entry
/// (e.g. prospero's `AgentsSettled`, ADR 0005) belongs to another field manager.
const OPERATOR_CONDITIONS: &[&str] = &["Ready"];

fn operator_conditions(s: &CalibanTaskStatus) -> Vec<&Condition> {
    s.conditions
        .iter()
        .filter(|c| OPERATOR_CONDITIONS.contains(&c.type_.as_str()))
        .collect()
}

fn status_eq(a: &CalibanTaskStatus, b: &CalibanTaskStatus) -> bool {
    a.phase == b.phase
        && a.caliband_endpoint == b.caliband_endpoint
        && a.sandbox_ref.as_ref().map(|r| &r.name) == b.sandbox_ref.as_ref().map(|r| &r.name)
        // Only the operator's own conditions: another manager's entry on the
        // observed status would otherwise make every reconcile re-apply.
        && operator_conditions(a) == operator_conditions(b)
}

/// Periodic re-reconcile interval (#55). The controller watches the Sandboxes it
/// owns, so a Sandbox status change (e.g. becoming Ready) triggers the task's
/// reconcile immediately; this timer is only a safety net against missed events.
pub(crate) const RESYNC: Duration = Duration::from_secs(300);

/// Status for a task that can't be provisioned: `Failed` with a `Ready=False`
/// condition carrying `reason` (e.g. `WorkspaceUnresolved`, `InvalidName`) and a
/// human-readable `message`. Returns `None` when the observed status already
/// records this exact failure (#55), so a permanently-failed task isn't
/// re-applied on every reconcile — the same no-op discipline as `derive_status`.
pub(crate) fn derive_failed_status(
    t: &CalibanTask,
    reason: &str,
    message: &str,
) -> Option<CalibanTaskStatus> {
    let next = CalibanTaskStatus {
        phase: Phase::Failed,
        conditions: vec![Condition {
            type_: "Ready".into(),
            status: "False".into(),
            reason: Some(reason.to_string()),
            message: Some(message.to_string()),
        }],
        ..Default::default()
    };
    match &t.status {
        Some(cur) if status_eq(cur, &next) => None,
        _ => Some(next),
    }
}

/// Force-apply the operator's status fields under the `caliban-operator` field
/// manager (#64, ADR 0005). `force` takes ownership from the legacy merge-patch
/// manager on existing tasks; it cannot steal another component's fields
/// because the body only ever carries operator-owned ones.
async fn apply_status(
    api: &Api<CalibanTask>,
    name: &str,
    status: &CalibanTaskStatus,
) -> Result<(), Error> {
    let pp = PatchParams::apply("caliban-operator").force();
    api.patch_status(name, &pp, &Patch::Apply(status_apply_body(status)))
        .await?;
    Ok(())
}

/// Server-side apply `obj` under `name`, force-owned by the operator's field
/// manager. Used for the operator's own children (SA, NetworkPolicy, Sandbox).
async fn apply<K>(api: &Api<K>, name: &str, obj: &K) -> Result<K, Error>
where
    K: Clone + std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned + Resource,
    K::DynamicType: Default,
{
    let pp = PatchParams::apply("caliban-operator").force();
    // The apply response is the live object (status included), so callers
    // that need observed state don't have to re-GET it (#55).
    Ok(api.patch(name, &pp, &Patch::Apply(obj)).await?)
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

    // #49: without a UID the children can't carry an owner reference; building
    // one with an empty UID only gets it rejected by the API server.
    if obj.uid().is_none() {
        return Err(Error::MissingUid(name));
    }
    // #49: a name too long for the Sandbox's Service fails every apply, and
    // names are immutable — fail once with a clear reason and wait for the
    // object to change rather than retrying a doomed apply.
    if let Some(problem) = task_name_problem(&obj) {
        if let Some(failed) = derive_failed_status(&obj, "InvalidName", &problem) {
            let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
            apply_status(&task_api, &name, &failed).await?;
            tracing::warn!(%ns, %name, %problem, "invalid task name; task Failed");
            let ev = events::task_failed("InvalidName", &problem);
            events::publish(&ctx.recorder, &obj.object_ref(&()), ev).await;
        }
        return Ok(Action::await_change());
    }

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
                    let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                    let pin = CalibanTaskStatus {
                        resolved_workspace: Some(rw.clone()),
                        ..Default::default()
                    };
                    apply_status(&task_api, &name, &pin).await?;
                    rw
                }
                Admission::Requeue => {
                    // Workspace exists but isn't Ready yet; don't pin. Its own
                    // controller will validate it shortly — re-check soon.
                    tracing::info!(%ns, %name, "workspace not ready; deferring pin");
                    return Ok(Action::requeue(Duration::from_secs(10)));
                }
                Admission::Fail(reason) => {
                    if let Some(failed) = derive_failed_status(&obj, "WorkspaceUnresolved", &reason)
                    {
                        let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
                        apply_status(&task_api, &name, &failed).await?;
                        tracing::warn!(%ns, %name, "workspace unresolved; task Failed");
                        let ev = events::task_failed("WorkspaceUnresolved", &reason);
                        events::publish(&ctx.recorder, &obj.object_ref(&()), ev).await;
                    }
                    // Nothing watches the referenced Workspace from here, so keep
                    // re-checking on a short interval: fixing the workspace is
                    // what recovers this task.
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
    let sandbox = apply(&sb_api, &p.sandbox.name_any(), &p.sandbox).await?;

    // Derive + apply CalibanTask status from the Sandbox the apply returned.
    if let Some(mut status) = derive_status(&obj, Some(&sandbox), s) {
        let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
        let phase = status.phase;
        let previous = obj.status.as_ref().map(|st| st.phase);
        // The pin is an operator-owned field too. Under server-side apply,
        // omitting it would remove it — and `obj` predates a pin applied
        // earlier in this same reconcile — so always carry the pinned config.
        status.resolved_workspace = Some(resolved.clone());
        apply_status(&task_api, &name, &status).await?;
        tracing::info!(%ns, %name, ?phase, "patched CalibanTask status");
        if let Some(ev) = events::phase_changed(previous, phase) {
            events::publish(&ctx.recorder, &obj.object_ref(&()), ev).await;
        }
    }
    Ok(Action::requeue(RESYNC))
}

fn error_policy(obj: Arc<CalibanTask>, err: &Error, ctx: Arc<Context>) -> Action {
    tracing::warn!(error = %err, "reconcile error");
    // The error policy is synchronous; publish the Warning on its own task so
    // it never delays the requeue decision (#56).
    let ev = events::reconcile_error(err);
    let reference = obj.object_ref(&());
    tokio::spawn(async move {
        events::publish(&ctx.recorder, &reference, ev).await;
    });
    Action::requeue(Duration::from_secs(30))
}

/// Run the CalibanTask controller until shutdown.
pub async fn run(client: Client) -> anyhow::Result<()> {
    let tasks: Api<CalibanTask> = Api::all(client.clone());
    let sandboxes: Api<Sandbox> = Api::all(client.clone());
    let settings = Settings::from_env();
    // #49: fail fast on settings that would otherwise break every reconcile.
    settings.validate().map_err(anyhow::Error::msg)?;
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: "caliban-operator".to_string(),
            instance: std::env::var("CONTROLLER_POD_NAME").ok(),
        },
    );
    let ctx = Arc::new(Context {
        client,
        settings,
        recorder,
    });
    Controller::new(tasks, Config::default())
        // Watch the Sandboxes we own (#55): a Sandbox status change — becoming
        // Ready, losing readiness — maps back through its controller owner
        // reference and reconciles the task at once, instead of waiting out a
        // polling interval. RESYNC is only the safety net.
        .owns(sandboxes, Config::default())
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
    use crate::crd::{CalibanTaskSpec, TaskSpec, WorkspaceRef};
    use crate::sandbox::{SandboxSpec, SandboxStatus};
    use crate::workspace::Source;
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
                    interactive: None,
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

    /// A Sandbox as agent-sandbox reports it: `fqdn`, plus a `Ready` condition
    /// with the given status/reason/message when `ready` is `Some`.
    fn sandbox_status(fqdn: Option<&str>, ready: Option<(&str, &str, &str)>) -> Sandbox {
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
            conditions: ready
                .map(|(status, reason, message)| {
                    vec![Condition {
                        type_: "Ready".into(),
                        status: status.into(),
                        reason: Some(reason.into()),
                        message: Some(message.into()),
                    }]
                })
                .unwrap_or_default(),
        });
        sb
    }

    /// A Sandbox with `fqdn` that agent-sandbox reports *ready* whenever it has
    /// one — the healthy case the pre-#45 tests below describe.
    fn sandbox_with_fqdn(fqdn: Option<&str>) -> Sandbox {
        let ready = fqdn.map(|_| ("True", "DependenciesReady", "Pod is Ready"));
        sandbox_status(fqdn, ready)
    }

    /// #45: the Service (and so `serviceFQDN`) exists before caliband can
    /// answer. A Sandbox whose own `Ready` is not True must not make the task
    /// `Running` — prosperod would dial a dead endpoint.
    #[test]
    fn sandbox_with_fqdn_but_not_ready_is_provisioning_with_ready_false() {
        let t = task_without_status();
        let sb = sandbox_status(
            Some("refactor-auth-sbx.team-a.svc"),
            Some((
                "False",
                "DependenciesNotReady",
                "Pod is Running but not Ready",
            )),
        );
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Provisioning);
        assert!(d.caliband_endpoint.is_none(), "no endpoint until ready");
        assert_eq!(d.conditions.len(), 1);
        let c = &d.conditions[0];
        assert_eq!(c.type_, "Ready");
        assert_eq!(c.status, "False");
        assert_eq!(c.reason.as_deref(), Some("SandboxNotReady"));
        assert_eq!(c.message.as_deref(), Some("Pod is Running but not Ready"));
    }

    #[test]
    fn sandbox_with_fqdn_and_no_ready_condition_yet_is_provisioning() {
        // agent-sandbox hasn't computed readiness yet: not evidence of Ready.
        let t = task_without_status();
        let sb = sandbox_status(Some("refactor-auth-sbx.team-a.svc"), None);
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Provisioning);
        assert!(d.caliband_endpoint.is_none());
    }

    #[test]
    fn running_task_whose_sandbox_loses_readiness_drops_back_to_provisioning() {
        let mut t = task_without_status();
        let fqdn = Some("refactor-auth-sbx.team-a.svc");
        t.status = super::derive_status(&t, Some(&sandbox_with_fqdn(fqdn)), &Settings::default());
        assert_eq!(t.status.as_ref().unwrap().phase, Phase::Running);
        // caliband crash-loops: the pod is no longer Ready.
        let sb = sandbox_status(
            fqdn,
            Some((
                "False",
                "DependenciesNotReady",
                "Pod is Running but not Ready",
            )),
        );
        let d = super::derive_status(&t, Some(&sb), &Settings::default())
            .expect("losing readiness is a status change");
        assert_eq!(d.phase, Phase::Provisioning);
        assert!(d.caliband_endpoint.is_none());
        assert_eq!(d.conditions[0].status, "False");
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

    /// A condition another field manager (prospero, ADR 0005) owns on the CR.
    fn agents_settled() -> Condition {
        Condition {
            type_: "AgentsSettled".into(),
            status: "True".into(),
            reason: Some("Succeeded".into()),
            message: None,
        }
    }

    /// A condition as prospero applies it (prospero#228): `AgentsSettled` with a
    /// `Succeeded` / `Failed` / `AgentsActive` reason.
    fn settled(status: &str, reason: &str, message: Option<&str>) -> Condition {
        Condition {
            type_: "AgentsSettled".into(),
            status: status.into(),
            reason: Some(reason.into()),
            message: message.map(str::to_string),
        }
    }

    /// A Running task whose agents prospero has reported on.
    fn task_with_settled(c: Condition) -> (CalibanTask, Sandbox) {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        t.status.as_mut().unwrap().conditions.push(c);
        (t, sb)
    }

    /// #37: infrastructure alone cannot tell "agent running" from "agent
    /// finished, pod still up", so a finished task reported `Running` forever.
    /// prospero's `AgentsSettled` is what makes the phase terminal (ADR 0005).
    #[test]
    fn agents_settled_succeeded_makes_the_task_completed() {
        let (t, sb) = task_with_settled(settled("True", "Succeeded", None));
        let d = super::derive_status(&t, Some(&sb), &Settings::default())
            .expect("Running -> Completed is a change");
        assert_eq!(d.phase, Phase::Completed);
        let ready = &d.conditions[0];
        assert_eq!(ready.type_, "Ready");
        assert_eq!(ready.status, "False");
        assert_eq!(ready.reason.as_deref(), Some("Completed"));
    }

    #[test]
    fn agents_settled_failed_makes_the_task_failed_with_the_message() {
        let (t, sb) = task_with_settled(settled("True", "Failed", Some("agent exited 1")));
        let d = super::derive_status(&t, Some(&sb), &Settings::default())
            .expect("Running -> Failed is a change");
        assert_eq!(d.phase, Phase::Failed);
        assert_eq!(d.conditions[0].status, "False");
        assert_eq!(d.conditions[0].reason.as_deref(), Some("AgentsFailed"));
        assert_eq!(d.conditions[0].message.as_deref(), Some("agent exited 1"));
    }

    /// prospero's `AgentsSettled` never sets `message` today (prospero#228), so a
    /// failed task would otherwise show an empty reason in `kubectl describe`.
    #[test]
    fn a_failure_without_a_message_gets_a_fallback() {
        let (t, sb) = task_with_settled(settled("True", "Failed", None));
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        assert_eq!(d.phase, Phase::Failed);
        assert_eq!(
            d.conditions[0].message.as_deref(),
            Some("one or more agents failed or crashed")
        );
    }

    /// An idle interactive task is deliberately *not* settled: prospero reports
    /// `AgentsActive`, and the task stays Running with its endpoint.
    #[test]
    fn agents_active_leaves_the_task_running() {
        let (t, sb) = task_with_settled(settled("False", "AgentsActive", None));
        assert!(
            super::derive_status(&t, Some(&sb), &Settings::default()).is_none(),
            "no phase change, so no status write"
        );
        assert_eq!(t.status.as_ref().unwrap().phase, Phase::Running);
    }

    /// Only the two reasons in prospero's contract are terminal; anything else
    /// leaves the infrastructure-derived phase alone.
    #[test]
    fn a_settled_condition_with_an_unknown_reason_keeps_the_infrastructure_phase() {
        let (t, sb) = task_with_settled(settled("True", "Mysterious", None));
        assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
        assert_eq!(t.status.as_ref().unwrap().phase, Phase::Running);
    }

    /// Terminal is stable: re-reconciling a Completed task writes nothing.
    #[test]
    fn a_completed_task_does_not_churn() {
        let (mut t, sb) = task_with_settled(settled("True", "Succeeded", None));
        let mut completed = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        // The observed status carries both managers' conditions.
        completed
            .conditions
            .push(settled("True", "Succeeded", None));
        t.status = Some(completed);
        assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
    }

    /// #64 (ADR 0005): status is written by server-side apply, so the body is a
    /// partial object that must name its type.
    #[test]
    fn status_apply_body_names_the_object_type() {
        let body = super::status_apply_body(&CalibanTaskStatus::default());
        assert_eq!(body["apiVersion"], "caliban.caliban-ai.dev/v1alpha1");
        assert_eq!(body["kind"], "CalibanTask");
        assert!(body["status"].is_object());
    }

    /// Under server-side apply a manager releases (and the server removes) any
    /// field it owned but omits — the inverse of merge patch, which needed an
    /// explicit `null`/`[]`. A cleared endpoint, ref, and condition set must be
    /// *absent* from the apply, not null.
    #[test]
    fn cleared_endpoint_ref_and_conditions_are_omitted_from_the_apply() {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        let cleared = super::derive_status(&t, None, &Settings::default()).unwrap();
        let body = super::status_apply_body(&cleared);
        let st = &body["status"];
        assert_eq!(st["phase"], "Pending");
        assert!(st.get("calibandEndpoint").is_none(), "{st}");
        assert!(st.get("sandboxRef").is_none(), "{st}");
        assert!(st.get("conditions").is_none(), "{st}");
    }

    /// The operator force-applies status, so its body must never carry a
    /// condition another manager owns — force would steal it.
    #[test]
    fn the_apply_never_carries_a_condition_the_operator_does_not_own() {
        let mut t = task_without_status();
        t.status = Some(CalibanTaskStatus {
            conditions: vec![agents_settled()],
            ..Default::default()
        });
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        let d = super::derive_status(&t, Some(&sb), &Settings::default()).unwrap();
        let body = super::status_apply_body(&d);
        let types: Vec<&str> = body["status"]["conditions"]
            .as_array()
            .expect("operator conditions present")
            .iter()
            .map(|c| c["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, vec!["Ready"]);
    }

    /// Once prospero has applied `AgentsSettled`, the observed conditions never
    /// equal the operator's own set. Comparing them wholesale would re-apply
    /// status on every reconcile. (Uses a non-terminal `AgentsActive` condition:
    /// since #37 a `Succeeded`/`Failed` one legitimately changes the phase.)
    #[test]
    fn a_condition_owned_by_another_manager_does_not_cause_status_churn() {
        let mut t = task_without_status();
        let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
        t.status = super::derive_status(&t, Some(&sb), &Settings::default());
        t.status
            .as_mut()
            .unwrap()
            .conditions
            .push(settled("False", "AgentsActive", None));
        assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
    }

    /// Each field manager owns its own `conditions` entries only if the list is
    /// a map keyed by `type`; as an atomic list, one apply replaces all of it.
    #[test]
    fn task_status_conditions_is_a_map_list_keyed_by_type() {
        use kube::CustomResourceExt;
        let crd = serde_json::to_value(CalibanTask::crd()).unwrap();
        let c = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["status"]
            ["properties"]["conditions"];
        assert_eq!(c["x-kubernetes-list-type"], "map", "{c}");
        assert_eq!(c["x-kubernetes-list-map-keys"], serde_json::json!(["type"]));
    }

    /// #55: the admission-failure path re-applied an identical status on every
    /// reconcile, for every permanently-failed task. Like `derive_status`, it
    /// must skip a write that changes nothing.
    #[test]
    fn a_repeated_admission_failure_with_the_same_reason_is_not_reapplied() {
        let mut t = task_without_status();
        let reason = "Workspace not found: 'team-a-ws'";
        t.status = super::derive_failed_status(&t, "WorkspaceUnresolved", reason);
        assert_eq!(t.status.as_ref().unwrap().phase, Phase::Failed);
        assert!(super::derive_failed_status(&t, "WorkspaceUnresolved", reason).is_none());
    }

    #[test]
    fn a_changed_admission_failure_reason_is_applied() {
        let mut t = task_without_status();
        t.status = super::derive_failed_status(
            &t,
            "WorkspaceUnresolved",
            "Workspace not found: 'team-a-ws'",
        );
        let next = "workspace 'team-a-ws' not ready: validation failed";
        let d = super::derive_failed_status(&t, "WorkspaceUnresolved", next)
            .expect("a new reason is a change");
        assert_eq!(d.phase, Phase::Failed);
        assert_eq!(d.conditions.len(), 1);
        assert_eq!(d.conditions[0].status, "False");
        assert_eq!(
            d.conditions[0].reason.as_deref(),
            Some("WorkspaceUnresolved")
        );
        assert_eq!(d.conditions[0].message.as_deref(), Some(next));
    }

    /// #55: the Sandbox is watched (`owns`), so its status change triggers the
    /// reconcile directly; the periodic requeue is only a safety net.
    #[test]
    fn periodic_resync_is_a_safety_net_not_the_trigger() {
        assert_eq!(super::RESYNC, Duration::from_secs(300));
    }

    /// #49: a task that fails for a reason other than its workspace carries its
    /// own reason code, so the cause is legible in `kubectl describe`.
    #[test]
    fn a_failure_carries_its_own_reason_code() {
        let t = task_without_status();
        let d = super::derive_failed_status(&t, "InvalidName", "too long").unwrap();
        assert_eq!(d.phase, Phase::Failed);
        assert_eq!(d.conditions[0].reason.as_deref(), Some("InvalidName"));
        assert_eq!(d.conditions[0].message.as_deref(), Some("too long"));
    }

    /// #49: a task with no UID cannot own its children. That is a named error,
    /// not an owner reference with an empty UID the API server rejects.
    #[test]
    fn a_missing_uid_is_a_named_error() {
        let e = Error::MissingUid("refactor-auth".into());
        let msg = e.to_string();
        assert!(msg.contains("refactor-auth"), "{msg}");
        assert!(msg.contains("uid"), "{msg}");
    }
}
