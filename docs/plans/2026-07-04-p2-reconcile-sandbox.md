# CalibanTask → Sandbox Reconcile Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the operator's reconcile turn a `CalibanTask` into a running, sandboxed caliband pod reachable at a stable DNS, with a per-task token-less ServiceAccount and a default-deny NetworkPolicy, and drive `.status` (phase + `calibandEndpoint`) from the backing agent-sandbox `Sandbox`.

**Architecture:** The reconcile is **pure builders + a thin apply loop** (ADR 0002). Pure functions map `CalibanTask → {Sandbox, ServiceAccount, NetworkPolicy}` and derive status; the async reconcile server-side-applies each child (owner-referenced for GC), reads the Sandbox back, and patches `CalibanTask.status`. The `Sandbox` (`agents.x-k8s.io/v1beta1`) is consumed as a hand-authored foreign typed resource — we never emit its CRD.

**Tech Stack:** Rust, kube 4.0 (`runtime`/`derive`/`client`), k8s-openapi 0.28 (`v1_32` + **`schemars`** feature), schemars 1.2, serde, tokio.

## Global Constraints

- **Do not reimplement pods/isolation** — compose agent-sandbox's `Sandbox`; the operator is a thin caliban-semantics layer (ADR 0002 §Decision 1).
- **Cluster-agnostic** — no home-cluster identifiers (hostnames, domains, IPs, storage classes, ingress classes, node selectors) in any generated object. Environment-specifics come from operator config with neutral defaults (`storageClass` omitted → cluster default).
- **Idempotent reconcile** — server-side apply (`Patch::Apply`, field manager `"caliban-operator"`, `.force()`) + a controller `OwnerReference` on every child. Deterministic child names: `<task>-sbx`, `<task>-sa`, `<task>-netpol`.
- **Least-privilege pod identity** — per-task `ServiceAccount` with `automountServiceAccountToken: false` and **no** bound Role (zero API rights). Referenced from the podTemplate.
- **caliband network contract** — the pod listens TCP+TLS on port **8443**; the Sandbox sets `service: true` so agent-sandbox provisions the Service whose `serviceFQDN` is the session endpoint. `calibandEndpoint = "<serviceFQDN>:8443"`.
- **Do not emit or drift-guard the `Sandbox` CRD** — agent-sandbox owns it. `crdgen` and the committed `deploy/crd/*.yaml` remain CalibanTask-only.
- Pure builders/derivation are **unit-tested with no cluster**. The async apply glue is thin.
- Preserve existing behavior: `desired_status`'s no-op-when-unchanged discipline (#282) carries into `derive_status`.

---

### Task 1: Foreign `Sandbox` resource type (`agents.x-k8s.io/v1beta1`)

**Files:**
- Modify: `Cargo.toml` (add `schemars` to k8s-openapi features)
- Create: `src/sandbox.rs`
- Modify: `src/lib.rs` (add `pub mod sandbox;`)

**Interfaces:**
- Produces: `Sandbox` (kube `CustomResource`-derived root), `SandboxSpec`, `SandboxStatus`. Used by Tasks 4/5/6.

**Context:** agent-sandbox's verified v1beta1 `SandboxSpec` = `{ podTemplate{metadata,spec} (required), service bool, operatingMode enum(Running|Suspended), shutdownPolicy enum(Delete|Retain), shutdownTime string, volumeClaimTemplates []core.PVC }`; `SandboxStatus` = `{ serviceFQDN, service, podIPs, nodeName, conditions }`. We declare only the fields we set (`podTemplate`, `service`, `operatingMode`, `volumeClaimTemplates`) or read (`serviceFQDN`, `conditions`); omitted fields are pruned by the API server. `podTemplate` reuses `k8s_openapi::api::core::v1::PodTemplateSpec`; `volumeClaimTemplates` reuses `PersistentVolumeClaim`. Deriving `CustomResource` requires `JsonSchema`, which is why k8s-openapi needs its `schemars` feature (uses schemars `1` — matches our 1.2). We never call `Sandbox::crd()`.

- [ ] **Step 1: Enable k8s-openapi schemars feature**

In `Cargo.toml`, change:
```toml
k8s-openapi = { version = "0.28.0", default-features = false, features = ["v1_32", "schemars"] }
```

- [ ] **Step 2: Write `src/sandbox.rs`**

```rust
//! A minimal foreign view of agent-sandbox's `Sandbox` CRD
//! (`agents.x-k8s.io/v1beta1`). We consume it — we declare only the fields the
//! operator sets or reads; the API server prunes the rest. We never emit this
//! CRD (agent-sandbox owns it), so `Sandbox::crd()` is intentionally unused. See
//! ADR 0002.

use k8s_openapi::api::core::v1::{PersistentVolumeClaim, PodTemplateSpec};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Desired state of an agent-sandbox `Sandbox` (subset the operator manages).
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "agents.x-k8s.io",
    version = "v1beta1",
    kind = "Sandbox",
    namespaced,
    status = "SandboxStatus"
)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSpec {
    /// Pod template for the sandbox's single pod.
    pub pod_template: PodTemplateSpec,
    /// Provision a headless Service (→ stable `serviceFQDN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<bool>,
    /// `Running` or `Suspended`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operating_mode: Option<String>,
    /// PVCs materialized for the sandbox (persist across restarts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_claim_templates: Option<Vec<PersistentVolumeClaim>>,
}

/// Observed state of a `Sandbox` (subset the operator reads).
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatus {
    /// Stable in-cluster DNS name for the sandbox's Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_fqdn: Option<String>,
}
```

- [ ] **Step 3: Register the module** — add `pub mod sandbox;` to `src/lib.rs`.

- [ ] **Step 4: Write the round-trip test** (in `src/sandbox.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_serializes_with_group_version_kind() {
        let sb = Sandbox::new(
            "demo-sbx",
            SandboxSpec {
                pod_template: PodTemplateSpec::default(),
                service: Some(true),
                operating_mode: Some("Running".to_string()),
                volume_claim_templates: None,
            },
        );
        let v = serde_json::to_value(&sb).unwrap();
        assert_eq!(v["apiVersion"], "agents.x-k8s.io/v1beta1");
        assert_eq!(v["kind"], "Sandbox");
        assert_eq!(v["spec"]["service"], true);
        assert_eq!(v["spec"]["operatingMode"], "Running");
        assert!(v["spec"]["podTemplate"].is_object());
    }

    #[test]
    fn status_reads_service_fqdn() {
        let json = serde_json::json!({ "serviceFQDN": "demo-sbx.team-a.svc" });
        let st: SandboxStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st.service_fqdn.as_deref(), Some("demo-sbx.team-a.svc"));
    }
}
```

- [ ] **Step 5: Run** `cargo test --lib sandbox` → PASS. Then `cargo run --bin crdgen > /tmp/x.yaml && diff <(cat deploy/crd/calibantask.yaml) /tmp/x.yaml` → **no diff** (enabling schemars must not change the CalibanTask CRD).

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat(operator): foreign Sandbox type (agents.x-k8s.io/v1beta1) (#283)"`

---

### Task 2: Operator settings + object identity helpers

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs` (add `pub mod config;`)

**Interfaces:**
- Produces:
  - `Settings { caliband_image: String, caliband_port: i32, workspace_root: String, workspace_storage: String }` with `Settings::from_env()` and `Settings::default()`.
  - `fn sandbox_name(t: &CalibanTask) -> String` (`<name>-sbx`), `sa_name` (`<name>-sa`), `netpol_name` (`<name>-netpol`).
  - `fn common_labels(t: &CalibanTask) -> BTreeMap<String,String>`.
  - `fn owner_ref(t: &CalibanTask) -> OwnerReference`.
- Consumed by Tasks 3/4/5/6.

**Context:** The caliband image is operator config, not CR intent (the CR declares intent). Defaults must be neutral/cluster-agnostic. `owner_ref` needs the task's `uid` (present on any cluster-fetched object; in unit tests we set it explicitly).

- [ ] **Step 1: Write `src/config.rs`**

```rust
//! Operator-level settings (image, ports, workspace defaults) and helpers for
//! naming and owning the child objects a reconcile creates. See ADR 0002.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::{Resource, ResourceExt};

use crate::crd::CalibanTask;

/// Runtime configuration, sourced from the environment with neutral defaults.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Container image for the caliband pod.
    pub caliband_image: String,
    /// TCP+TLS port caliband listens on inside the pod.
    pub caliband_port: i32,
    /// Workspace root mount path in the pod.
    pub workspace_root: String,
    /// Requested size of the workspace PVC (e.g. `10Gi`).
    pub workspace_storage: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            caliband_image: "ghcr.io/caliban-ai/caliban:latest".to_string(),
            caliband_port: 8443,
            workspace_root: "/work".to_string(),
            workspace_storage: "10Gi".to_string(),
        }
    }
}

impl Settings {
    /// Read settings from `CALIBAND_IMAGE`, `CALIBAND_PORT`, `CALIBAN_WORKSPACE_ROOT`,
    /// `CALIBAN_WORKSPACE_STORAGE`, falling back to defaults.
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            caliband_image: std::env::var("CALIBAND_IMAGE").unwrap_or(d.caliband_image),
            caliband_port: std::env::var("CALIBAND_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.caliband_port),
            workspace_root: std::env::var("CALIBAN_WORKSPACE_ROOT").unwrap_or(d.workspace_root),
            workspace_storage: std::env::var("CALIBAN_WORKSPACE_STORAGE")
                .unwrap_or(d.workspace_storage),
        }
    }
}

/// Name of the Sandbox backing a task.
pub fn sandbox_name(t: &CalibanTask) -> String {
    format!("{}-sbx", t.name_any())
}
/// Name of the task's dedicated ServiceAccount.
pub fn sa_name(t: &CalibanTask) -> String {
    format!("{}-sa", t.name_any())
}
/// Name of the task's NetworkPolicy.
pub fn netpol_name(t: &CalibanTask) -> String {
    format!("{}-netpol", t.name_any())
}

/// Labels stamped on every child object, keyed to the owning task.
pub fn common_labels(t: &CalibanTask) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "app.kubernetes.io/managed-by".to_string(),
            "caliban-operator".to_string(),
        ),
        (
            "caliban.caliban-ai.dev/task".to_string(),
            t.name_any(),
        ),
    ])
}

/// A controller owner reference to the task, so children cascade-delete.
pub fn owner_ref(t: &CalibanTask) -> OwnerReference {
    OwnerReference {
        api_version: CalibanTask::api_version(&()).to_string(),
        kind: CalibanTask::kind(&()).to_string(),
        name: t.name_any(),
        uid: t.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}
```

- [ ] **Step 2: Register the module** — add `pub mod config;` to `src/lib.rs`.

- [ ] **Step 3: Write tests** (in `src/config.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::{CalibanTask, CalibanTaskSpec, Source, TaskSpec, Workspace};

    fn task() -> CalibanTask {
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
                task: TaskSpec { prompt: "hi".into(), agent_type: None },
                model: None, state: None, isolation: None, resources: None, lifecycle: None,
            },
        );
        t.metadata.namespace = Some("team-a".into());
        t.metadata.uid = Some("uid-123".into());
        t
    }

    #[test]
    fn names_are_deterministic() {
        let t = task();
        assert_eq!(sandbox_name(&t), "refactor-auth-sbx");
        assert_eq!(sa_name(&t), "refactor-auth-sa");
        assert_eq!(netpol_name(&t), "refactor-auth-netpol");
    }

    #[test]
    fn owner_ref_is_controller_with_uid() {
        let o = owner_ref(&task());
        assert_eq!(o.kind, "CalibanTask");
        assert_eq!(o.api_version, "caliban.caliban-ai.dev/v1alpha1");
        assert_eq!(o.name, "refactor-auth");
        assert_eq!(o.uid, "uid-123");
        assert_eq!(o.controller, Some(true));
    }

    #[test]
    fn from_env_defaults_are_neutral() {
        let s = Settings::default();
        assert_eq!(s.caliband_port, 8443);
        assert!(!s.caliband_image.contains("home"));
        assert_eq!(s.workspace_root, "/work");
    }
}
```

- [ ] **Step 4: Run** `cargo test --lib config` → PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(operator): reconcile settings + child naming/owner helpers (#283)"`

---

### Task 3: Build the ServiceAccount and NetworkPolicy

**Files:**
- Create: `src/resources.rs`
- Modify: `src/lib.rs` (add `pub mod resources;`)

**Interfaces:**
- Consumes: `config::{sa_name, netpol_name, common_labels, owner_ref, Settings}`.
- Produces: `fn build_service_account(t: &CalibanTask) -> ServiceAccount`, `fn build_network_policy(t: &CalibanTask, s: &Settings) -> NetworkPolicy`. Consumed by Task 5 (`plan`).

**Context:** Least-privilege pod identity = token-less SA, no Role (ADR 0002 §4). The NetworkPolicy (ADR 0002 §5) is default-deny ingress+egress on the pod's own labels, plus: allow DNS egress (UDP+TCP 53), allow general egress (empty `to` = all destinations, for git clone / provider APIs), allow ingress on the caliband port. Selectors use the sandbox pod's labels — agent-sandbox stamps the Sandbox's own labels onto its pod; we select on our `common_labels`, which the Sandbox propagates via its podTemplate metadata (set in Task 4). No home-cluster identifiers.

- [ ] **Step 1: Write `build_service_account` + `build_network_policy` in `src/resources.rs`**

```rust
//! Pure builders mapping a `CalibanTask` to the child objects a reconcile
//! applies: a token-less ServiceAccount, a default-deny NetworkPolicy, and the
//! backing agent-sandbox Sandbox. No cluster access — unit-tested. See ADR 0002.

use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::ServiceAccount;
use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPort,
    NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::ResourceExt;

use crate::config::{common_labels, netpol_name, owner_ref, sa_name, Settings};
use crate::crd::CalibanTask;

fn child_meta(t: &CalibanTask, name: String, labels: BTreeMap<String, String>) -> ObjectMeta {
    ObjectMeta {
        name: Some(name),
        namespace: t.namespace(),
        labels: Some(labels),
        owner_references: Some(vec![owner_ref(t)]),
        ..Default::default()
    }
}

/// A dedicated, token-less ServiceAccount — the pod's least-privilege identity.
pub fn build_service_account(t: &CalibanTask) -> ServiceAccount {
    ServiceAccount {
        metadata: child_meta(t, sa_name(t), common_labels(t)),
        automount_service_account_token: Some(false),
        ..Default::default()
    }
}

fn np_port(proto: &str, port: i32) -> NetworkPolicyPort {
    NetworkPolicyPort {
        protocol: Some(proto.to_string()),
        port: Some(IntOrString::Int(port)),
        ..Default::default()
    }
}

/// Default-deny NetworkPolicy: allow DNS + general egress + caliband-port ingress.
pub fn build_network_policy(t: &CalibanTask, s: &Settings) -> NetworkPolicy {
    NetworkPolicy {
        metadata: child_meta(t, netpol_name(t), common_labels(t)),
        spec: Some(NetworkPolicySpec {
            // Select the sandbox pod by the labels we propagate into its template.
            pod_selector: LabelSelector {
                match_labels: Some(common_labels(t)),
                ..Default::default()
            },
            policy_types: Some(vec!["Ingress".to_string(), "Egress".to_string()]),
            // Ingress: caliband port only.
            ingress: Some(vec![NetworkPolicyIngressRule {
                ports: Some(vec![np_port("TCP", s.caliband_port)]),
                ..Default::default()
            }]),
            // Egress: DNS (53 UDP+TCP), then everything else (git/providers).
            egress: Some(vec![
                NetworkPolicyEgressRule {
                    ports: Some(vec![np_port("UDP", 53), np_port("TCP", 53)]),
                    ..Default::default()
                },
                NetworkPolicyEgressRule::default(),
            ]),
        }),
    }
}
```

- [ ] **Step 2: Register the module** — add `pub mod resources;` to `src/lib.rs`.

- [ ] **Step 3: Write tests** (in `src/resources.rs`; reuse a `task()` helper like Task 2's — define a local `fn task() -> CalibanTask` in this test module)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // ... local `fn task() -> CalibanTask { ... }` (namespace "team-a", uid set) ...

    #[test]
    fn service_account_is_token_less_and_owned() {
        let sa = build_service_account(&task());
        assert_eq!(sa.metadata.name.as_deref(), Some("refactor-auth-sa"));
        assert_eq!(sa.metadata.namespace.as_deref(), Some("team-a"));
        assert_eq!(sa.automount_service_account_token, Some(false));
        let owners = sa.metadata.owner_references.unwrap();
        assert_eq!(owners[0].controller, Some(true));
        assert_eq!(owners[0].kind, "CalibanTask");
    }

    #[test]
    fn network_policy_is_default_deny_with_dns_and_caliband_ingress() {
        let np = build_network_policy(&task(), &Settings::default());
        let spec = np.spec.unwrap();
        assert_eq!(
            spec.policy_types.as_ref().unwrap(),
            &vec!["Ingress".to_string(), "Egress".to_string()]
        );
        // Ingress allows the caliband port.
        let iports = spec.ingress.unwrap()[0].ports.clone().unwrap();
        assert!(iports.iter().any(|p| p.port == Some(IntOrString::Int(8443))));
        // Egress: DNS rule + an allow-all rule (empty `to`).
        let egress = spec.egress.unwrap();
        assert_eq!(egress.len(), 2);
        assert!(egress[1].to.is_none()); // allow-all destinations
        // Selects the pod by our managed labels.
        assert!(spec
            .pod_selector
            .match_labels
            .unwrap()
            .contains_key("caliban.caliban-ai.dev/task"));
    }
}
```

- [ ] **Step 4: Run** `cargo test --lib resources` → PASS.

- [ ] **Step 5: Commit** — `git commit -am "feat(operator): build token-less SA + default-deny NetworkPolicy (#283)"`

---

### Task 4: Build the Sandbox (podTemplate, workspace PVC, env)

**Files:**
- Modify: `src/resources.rs` (add `build_sandbox`)

**Interfaces:**
- Consumes: `sandbox::{Sandbox, SandboxSpec}`, `config::{sandbox_name, sa_name, common_labels, owner_ref, Settings}`, the `CalibanTask` spec.
- Produces: `fn build_sandbox(t: &CalibanTask, s: &Settings) -> Sandbox`. Consumed by Task 5 (`plan`).

**Context:** The podTemplate carries one caliband container (image + port `s.caliband_port`, env, workspace volume mount at `s.workspace_root`), the task's `runtimeClass` (from `spec.isolation.runtime_class`, else unset → cluster default), the token-less SA (`sa_name`, automount off), and `common_labels` on the pod (so the NetworkPolicy selects it). The workspace PVC is a `volumeClaimTemplates` entry (RWO, `s.workspace_storage`, storageClass unset → cluster default). `service: true`; `operatingMode: Running`. Env projected: `CALIBAND_LISTEN=tcp://0.0.0.0:<port>`, `CALIBAN_WORKSPACE_ROOT=<root>`, `CALIBAN_WORKSPACE_SOURCES=<json of sources>`, and `GONZALO_ENDPOINT` if `spec.state.gonzalo_endpoint` is set. The operator does not clone sources — caliband does (workspace-scoped caliband, caliban#281); env is the provisioning contract.

- [ ] **Step 1: Add `build_sandbox` (and helpers) to `src/resources.rs`**

```rust
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec, PodSpec,
    PodTemplateSpec, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use crate::config::{sandbox_name, Settings};
use crate::sandbox::{Sandbox, SandboxSpec};

const WORKSPACE_VOLUME: &str = "workspace";

fn env(name: &str, value: String) -> EnvVar {
    EnvVar { name: name.to_string(), value: Some(value), ..Default::default() }
}

fn caliband_env(t: &CalibanTask, s: &Settings) -> Vec<EnvVar> {
    let sources = serde_json::to_string(&t.spec.workspace.sources).unwrap_or_else(|_| "[]".into());
    let mut e = vec![
        env("CALIBAND_LISTEN", format!("tcp://0.0.0.0:{}", s.caliband_port)),
        env("CALIBAN_WORKSPACE_ROOT", s.workspace_root.clone()),
        env("CALIBAN_WORKSPACE_SOURCES", sources),
    ];
    if let Some(ep) = t.spec.state.as_ref().and_then(|st| st.gonzalo_endpoint.clone()) {
        e.push(env("GONZALO_ENDPOINT", ep));
    }
    e
}

fn workspace_pvc(t: &CalibanTask, s: &Settings) -> PersistentVolumeClaim {
    PersistentVolumeClaim {
        metadata: ObjectMeta { name: Some(WORKSPACE_VOLUME.to_string()), ..Default::default() },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(s.workspace_storage.clone()),
                )])),
                ..Default::default()
            }),
            // storageClassName unset → cluster default (cluster-agnostic).
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Map a `CalibanTask` to its backing agent-sandbox `Sandbox`.
pub fn build_sandbox(t: &CalibanTask, s: &Settings) -> Sandbox {
    let labels = common_labels(t);
    let container = Container {
        name: "caliband".to_string(),
        image: Some(s.caliband_image.clone()),
        ports: Some(vec![ContainerPort {
            container_port: s.caliband_port,
            name: Some("caliband".to_string()),
            ..Default::default()
        }]),
        env: Some(caliband_env(t, s)),
        volume_mounts: Some(vec![VolumeMount {
            name: WORKSPACE_VOLUME.to_string(),
            mount_path: s.workspace_root.clone(),
            ..Default::default()
        }]),
        ..Default::default()
    };
    let pod_spec = PodSpec {
        containers: vec![container],
        runtime_class_name: t.spec.isolation.as_ref().and_then(|i| i.runtime_class.clone()),
        service_account_name: Some(sa_name(t)),
        automount_service_account_token: Some(false),
        ..Default::default()
    };
    let mut sb = Sandbox::new(
        &sandbox_name(t),
        SandboxSpec {
            pod_template: PodTemplateSpec {
                metadata: Some(ObjectMeta { labels: Some(labels.clone()), ..Default::default() }),
                spec: Some(pod_spec),
            },
            service: Some(true),
            operating_mode: Some("Running".to_string()),
            volume_claim_templates: Some(vec![workspace_pvc(t, s)]),
        },
    );
    sb.metadata.namespace = t.namespace();
    sb.metadata.labels = Some(labels);
    sb.metadata.owner_references = Some(vec![owner_ref(t)]);
    sb
}
```

- [ ] **Step 2: Write tests** (append to the `resources` test module)

```rust
#[test]
fn sandbox_has_caliband_container_pvc_and_service() {
    let s = Settings::default();
    let sb = build_sandbox(&task(), &s);
    assert_eq!(sb.metadata.name.as_deref(), Some("refactor-auth-sbx"));
    assert_eq!(sb.metadata.namespace.as_deref(), Some("team-a"));
    assert_eq!(sb.spec.service, Some(true));
    let pod = sb.spec.pod_template.spec.unwrap();
    assert_eq!(pod.service_account_name.as_deref(), Some("refactor-auth-sa"));
    assert_eq!(pod.automount_service_account_token, Some(false));
    let c = &pod.containers[0];
    assert_eq!(c.image.as_deref(), Some("ghcr.io/caliban-ai/caliban:latest"));
    assert_eq!(c.ports.as_ref().unwrap()[0].container_port, 8443);
    // Env projects the listen addr + workspace root.
    let env = c.env.as_ref().unwrap();
    assert!(env.iter().any(|e| e.name == "CALIBAND_LISTEN"
        && e.value.as_deref() == Some("tcp://0.0.0.0:8443")));
    assert!(env.iter().any(|e| e.name == "CALIBAN_WORKSPACE_ROOT"));
    // Workspace PVC present.
    let pvcs = sb.spec.volume_claim_templates.unwrap();
    assert_eq!(pvcs[0].metadata.name.as_deref(), Some("workspace"));
    // Pod carries the managed labels (so the NetworkPolicy selects it).
    assert!(sb.spec.pod_template.metadata.unwrap().labels.unwrap()
        .contains_key("caliban.caliban-ai.dev/task"));
}

#[test]
fn sandbox_runtime_class_from_isolation() {
    use crate::crd::IsolationSpec;
    let mut t = task();
    t.spec.isolation = Some(IsolationSpec {
        runtime_class: Some("gvisor".into()),
        worktrees: None,
    });
    let sb = build_sandbox(&t, &Settings::default());
    assert_eq!(
        sb.spec.pod_template.spec.unwrap().runtime_class_name.as_deref(),
        Some("gvisor")
    );
}
```

- [ ] **Step 3: Run** `cargo test --lib resources` → PASS.

- [ ] **Step 4: Commit** — `git commit -am "feat(operator): build caliband Sandbox from CalibanTask (#283)"`

---

### Task 5: Reconcile plan aggregator + status derivation

**Files:**
- Modify: `src/resources.rs` (add `ReconcilePlan` + `plan`)
- Modify: `src/controller.rs` (replace `desired_status` with `derive_status`)

**Interfaces:**
- Produces:
  - `struct ReconcilePlan { pub service_account: ServiceAccount, pub network_policy: NetworkPolicy, pub sandbox: Sandbox }` and `fn plan(t: &CalibanTask, s: &Settings) -> ReconcilePlan`.
  - `fn derive_status(t: &CalibanTask, sandbox: Option<&Sandbox>, s: &Settings) -> Option<CalibanTaskStatus>` — returns `Some(new)` only when it differs from `t.status`, else `None`.
- Consumed by Task 6 (reconcile).

**Context:** `derive_status` (ADR 0002 §6) supersedes #282's `desired_status`: `Provisioning` once we have applied a Sandbox but it has no `serviceFQDN`; `Running` with `calibandEndpoint = "<serviceFQDN>:<port>"` + `sandboxRef` once `serviceFQDN` is populated. `Pending` is the value before the first apply (when `sandbox` is `None`). Skip the patch when the derived status equals the observed one. Delete `desired_status` and its tests; move status logic here so the controller stays thin.

- [ ] **Step 1: Add `ReconcilePlan` + `plan` to `src/resources.rs`**

```rust
/// The child objects a single reconcile applies.
pub struct ReconcilePlan {
    pub service_account: ServiceAccount,
    pub network_policy: NetworkPolicy,
    pub sandbox: Sandbox,
}

/// Assemble every child object for a task (pure).
pub fn plan(t: &CalibanTask, s: &Settings) -> ReconcilePlan {
    ReconcilePlan {
        service_account: build_service_account(t),
        network_policy: build_network_policy(t, s),
        sandbox: build_sandbox(t, s),
    }
}
```

- [ ] **Step 2: Test `plan`** (append to `resources` tests)

```rust
#[test]
fn plan_names_all_three_children() {
    let p = plan(&task(), &Settings::default());
    assert_eq!(p.service_account.metadata.name.as_deref(), Some("refactor-auth-sa"));
    assert_eq!(p.network_policy.metadata.name.as_deref(), Some("refactor-auth-netpol"));
    assert_eq!(p.sandbox.metadata.name.as_deref(), Some("refactor-auth-sbx"));
}
```

- [ ] **Step 3: Replace `desired_status` with `derive_status` in `src/controller.rs`**

Remove `desired_status` and its four unit tests. Add:

```rust
use crate::config::{sandbox_name, Settings};
use crate::crd::{NamedRef, Phase};
use crate::sandbox::Sandbox;

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
        next.sandbox_ref = Some(NamedRef { name: sandbox_name(t) });
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
```

- [ ] **Step 4: Write `derive_status` tests** (in `controller.rs` tests)

```rust
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
    assert_eq!(d.caliband_endpoint.as_deref(), Some("refactor-auth-sbx.team-a.svc:8443"));
}

#[test]
fn unchanged_status_is_noop() {
    let mut t = task_without_status();
    let sb = sandbox_with_fqdn(Some("refactor-auth-sbx.team-a.svc"));
    // First derivation, then apply it as the observed status.
    t.status = super::derive_status(&t, Some(&sb), &Settings::default());
    assert!(super::derive_status(&t, Some(&sb), &Settings::default()).is_none());
}
```
(Add `task_without_status()` and `sandbox_with_fqdn(Option<&str>) -> Sandbox` test helpers: build a `CalibanTask` named `refactor-auth`/ns `team-a`/uid set with `status: None`; and a `Sandbox` whose `.status = Some(SandboxStatus { service_fqdn: ... })`.)

- [ ] **Step 5: Run** `cargo test --lib` → PASS (controller + resources).

- [ ] **Step 6: Commit** — `git commit -am "feat(operator): reconcile plan + Sandbox-driven status derivation (#283)"`

---

### Task 6: Wire the async reconcile (apply plan + patch status)

**Files:**
- Modify: `src/controller.rs` (rewrite `reconcile`, `Context`, `run`)
- Modify: `src/bin/caliban-operator.rs` (pass `Settings::from_env()`)
- Modify: `src/lib.rs` (re-export `sandbox`, `config`, `resources` as needed)

**Interfaces:**
- Consumes: `resources::plan`, `derive_status`, `sandbox::Sandbox`, `config::{Settings, sandbox_name}`.

**Context:** The reconcile applies the three children by server-side apply (owner-referenced), reads the Sandbox back, derives status, and patches it. `Context` gains `settings: Settings`. Apply uses `PatchParams::apply("caliban-operator").force()`. This glue is not unit-tested (no cluster); correctness rides on the pure tests + home-cluster QA. Keep it minimal and legible.

- [ ] **Step 1: Rewrite `reconcile`, `Context`, `run` in `src/controller.rs`**

```rust
use kube::api::{Patch, PatchParams};
use crate::config::{sandbox_name, Settings};
use crate::resources::plan;
use crate::sandbox::Sandbox;

/// Shared reconcile context.
pub struct Context {
    /// Kubernetes client.
    pub client: Client,
    /// Operator settings.
    pub settings: Settings,
}

async fn apply<K>(api: &Api<K>, name: &str, obj: &K) -> Result<(), Error>
where
    K: Clone + std::fmt::Debug + serde::Serialize + serde::de::DeserializeOwned,
{
    let pp = PatchParams::apply("caliban-operator").force();
    api.patch(name, &pp, &Patch::Apply(obj)).await?;
    Ok(())
}

async fn reconcile(obj: Arc<CalibanTask>, ctx: Arc<Context>) -> Result<Action, Error> {
    let ns = obj.namespace().unwrap_or_default();
    let name = obj.name_any();
    let s = &ctx.settings;
    let p = plan(&obj, s);

    // Apply children (idempotent SSA; owner refs cascade-delete).
    let sa_api: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&sa_api, &p.service_account.name_any(), &p.service_account).await?;
    let np_api: Api<NetworkPolicy> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&np_api, &p.network_policy.name_any(), &p.network_policy).await?;
    let sb_api: Api<Sandbox> = Api::namespaced(ctx.client.clone(), &ns);
    apply(&sb_api, &p.sandbox.name_any(), &p.sandbox).await?;

    // Read the Sandbox back for its status, then derive + patch CalibanTask status.
    let sandbox = sb_api.get_opt(&sandbox_name(&obj)).await?;
    if let Some(status) = super::controller::derive_status(&obj, sandbox.as_ref(), s) {
        let task_api: Api<CalibanTask> = Api::namespaced(ctx.client.clone(), &ns);
        let patch = serde_json::json!({ "status": status });
        task_api
            .patch_status(&name, &PatchParams::default(), &Patch::Merge(&patch))
            .await?;
        tracing::info!(%ns, %name, ?status.phase, "patched CalibanTask status");
    }
    Ok(Action::requeue(Duration::from_secs(30)))
}
```
(Adjust imports: `ServiceAccount`, `NetworkPolicy` from k8s-openapi; `derive_status` is in this module so call it directly. Add `serde::de::DeserializeOwned` bound imports as needed. `ResourceExt` is already imported.)

- [ ] **Step 2: Update `run` to build `Settings`**

```rust
pub async fn run(client: Client) -> anyhow::Result<()> {
    let tasks: Api<CalibanTask> = Api::all(client.clone());
    let ctx = Arc::new(Context { client, settings: Settings::from_env() });
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
```
(`src/bin/caliban-operator.rs` still calls `controller::run(client)`; no change needed there since `run` now reads env itself. Leave the bin as-is unless imports break.)

- [ ] **Step 3: Run the gate** — `cargo fmt --all`, then `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace --all-targets`, `cargo test --workspace`. All PASS.

- [ ] **Step 4: Regenerate + verify the CalibanTask CRD is unchanged** — `cargo run --bin crdgen > deploy/crd/calibantask.yaml && git diff --exit-code deploy/crd/calibantask.yaml` → no diff (the `committed_crd_yaml_is_in_sync` test also guards this).

- [ ] **Step 5: Commit** — `git commit -am "feat(operator): reconcile CalibanTask → Sandbox + SA + NetworkPolicy, status from serviceFQDN (#283)"`

---

## Notes for the executor

- **Deferred (ADR 0002 §7), do not implement here:** the drain finalizer, idle→pause/resume, `SandboxTemplate`/warm pools, full Secret/ConfigMap credential projection. Flag scope creep toward these as over-building.
- **Test helper duplication:** each test module needs a `task()`/`task_without_status()` builder. A small amount of per-module duplication is fine (test-local fixtures); do not build a shared test crate for it.
- **The operator's own cluster RBAC** (permission to create Sandboxes/SAs/NetworkPolicies, patch `calibantasks/status`) is #284 (the chart), not this ticket. ADR 0002 §Consequences fixes the verb set that chart must grant.
