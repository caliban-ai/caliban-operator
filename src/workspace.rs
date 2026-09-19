//! The `Workspace` custom resource (v1alpha1): durable, shared config — sources,
//! named providers (each with its own model + Secret reference), env, isolation —
//! that `CalibanTask`s reference by name. Owned and reconciled by the operator,
//! which is the sole reader of provider credential Secrets. See ADR 0004 and the
//! Prospero K8s Config Plane design.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{Condition, IsolationSpec};

// Lives here, not in `crd`, because only a `Workspace` has sources;
// `CalibanTaskSpec` references a workspace instead (#59). (`//` not `///` so
// this doesn't leak into the generated CRD schema description.)
/// A single source checkout in the workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    /// Source identifier (matches caliband's workspace source name).
    #[schemars(length(min = 1))]
    pub name: String,
    /// Git remote to clone.
    #[schemars(length(min = 1))]
    pub repo: String,
    /// Git ref to check out. Defaults to `main`.
    #[serde(default = "default_ref")]
    pub r#ref: String,
    /// Absolute checkout path in the pod (e.g. `/work/caliban`). Must be a
    /// directory strictly under the operator's workspace root (default
    /// `/work`) and distinct from every other source's path; otherwise the
    /// Workspace goes `Failed`, since a checkout outside the persistent volume
    /// is lost on every restart.
    #[schemars(length(min = 1))]
    pub path: String,
}

fn default_ref() -> String {
    "main".to_string()
}

/// Desired state of a workspace: sources + named providers + defaults.
#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "caliban.caliban-ai.dev",
    version = "v1alpha1",
    kind = "Workspace",
    namespaced,
    status = "WorkspaceStatus",
    shortname = "cws",
    printcolumn = r#"{"name":"Phase","type":"string","jsonPath":".status.phase"}"#,
    printcolumn = r#"{"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSpec {
    /// Human-friendly dashboard label.
    #[schemars(length(min = 1))]
    pub display_name: String,
    /// The workspace's git checkouts (0..N). Optional at creation: a workspace
    /// may be registered bare and have sources added later (#21). `providers`
    /// stays required — `resolve` needs a bindable provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    /// Named providers (1..N) agents in this workspace can bind to.
    #[schemars(length(min = 1))]
    pub providers: Vec<Provider>,
    /// Provider name agents get when they don't request one. Implicit when
    /// exactly one provider is defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// Non-secret environment injected into every agent pod.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvEntry>,
    /// Default isolation for agents launched against this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationSpec>,
    // A Workspace-level field, not part of the shared `IsolationSpec`:
    // `CalibanTaskSpec.isolation` is a per-run override that wins over the
    // workspace default, so an egress field there would let a task widen the
    // workspace's restriction (#58). (`//` so this stays out of the CRD.)
    /// Egress restriction for agent pods. Unset keeps allow-all egress; set, it
    /// allows DNS plus only the listed destinations. A task cannot override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressSpec>,
    /// What agents in this workspace may do. A task cannot override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_policy: Option<AgentPolicy>,
}

// #80 adds the first field; #51 extends this block with the typed caliban
// governance settings. (`//` so this stays out of the CRD.)
/// Workspace-wide policy for the agents a task launches.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentPolicy {
    /// Admit tasks whose `permissionPosture` is `unattended`, which run under
    /// a bypass profile with no human answering permission prompts. Defaults
    /// to false. Whoever can edit this Workspace controls it, so limit who has
    /// write access to Workspaces.
    #[serde(default)]
    pub allow_unattended: bool,
}

/// Workspace-wide egress restriction for agent pods.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EgressSpec {
    /// Destinations agent pods may reach besides DNS. This list replaces the
    /// default allow-all egress: include the workspace's git remotes, its model
    /// provider endpoints, and gonzalod if used. An empty list allows DNS only.
    #[serde(default)]
    pub allow: Vec<EgressRule>,
}

/// One allowed egress destination.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EgressRule {
    /// Destination CIDR, e.g. `192.0.2.0/24` or `2001:db8::/32`.
    #[schemars(length(min = 1))]
    pub cidr: String,
    /// TCP ports allowed to that CIDR. Empty allows every port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<i32>,
}

/// A named model provider bound within a workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// Provider identifier, unique within the workspace (e.g. `planner`).
    #[schemars(length(min = 1))]
    pub name: String,
    /// Provider kind (e.g. `anthropic`, `openai`, `google`).
    #[schemars(length(min = 1))]
    pub kind: String,
    /// Override base URL (e.g. `http://localhost:9292/v1`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default model for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reference to an existing Secret for this provider's API key. Keyless
    /// providers (e.g. a local `openai` endpoint) omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_ref: Option<CredentialsRef>,
}

/// A by-name reference to a key within an existing Kubernetes Secret.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsRef {
    /// Name of the Secret (same namespace).
    #[schemars(length(min = 1))]
    pub secret_name: String,
    /// Key within the Secret's data.
    #[schemars(length(min = 1))]
    pub key: String,
}

/// A non-secret environment entry.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntry {
    /// Variable name.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Variable value.
    pub value: String,
}

/// Observed state of a `Workspace`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    /// Lifecycle phase.
    #[serde(default)]
    pub phase: WorkspacePhase,
    /// Standard Kubernetes conditions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
    /// The `.metadata.generation` this status reflects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
    /// Human-readable detail (e.g. `provider 'planner': secret 'anthropic-key' not found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `Workspace` lifecycle phase.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
pub enum WorkspacePhase {
    /// Created, not yet reconciled.
    #[default]
    Pending,
    /// Valid — all providers and credential Secrets resolve.
    Ready,
    /// Invalid — see `message`.
    Failed,
}

/// Outcome of validating a `Workspace` against known Secret existence.
pub struct WorkspaceValidation {
    /// Derived phase (`Ready` or `Failed`).
    pub phase: WorkspacePhase,
    /// Human-readable failure detail, `None` when `Ready`.
    pub message: Option<String>,
}

/// Pure validation of a `WorkspaceSpec`: every source path a distinct directory
/// under `workspace_root`, unique provider names, a resolvable
/// `defaultProvider`, and an existing Secret key for every `credentialsRef`.
/// `secret_present(secret_name, key)` reports Secret-key existence (cluster
/// lookup is the caller's responsibility). First problem found wins.
pub fn validate_workspace(
    spec: &WorkspaceSpec,
    workspace_root: &str,
    secret_present: impl Fn(&str, &str) -> bool,
) -> WorkspaceValidation {
    use std::path::{Component, Path, PathBuf};

    fn failed(message: String) -> WorkspaceValidation {
        WorkspaceValidation {
            phase: WorkspacePhase::Failed,
            message: Some(message),
        }
    }

    // #46: the clone init container mounts the workspace PVC at the root, so a
    // source must land strictly beneath it. Compare by path *components*, not
    // string prefix — `/workspace/x` is not under `/work`. A `..` segment could
    // climb back out, and the root itself would collide with every source.
    let root = Path::new(workspace_root);
    let mut seen_paths: std::collections::BTreeMap<PathBuf, &str> = Default::default();
    for src in &spec.sources {
        let p = Path::new(&src.path);
        let under_root = p.is_absolute()
            && !p.components().any(|c| c == Component::ParentDir)
            && p.strip_prefix(root)
                .is_ok_and(|rest| rest.components().next().is_some());
        if !under_root {
            return failed(format!(
                "source '{}': path '{}' must be a directory under the workspace root '{}'",
                src.name, src.path, workspace_root
            ));
        }
        // Normalised so `/work/app` and `/work/app/` are recognised as one.
        let normalised: PathBuf = p.components().collect();
        if let Some(prev) = seen_paths.insert(normalised.clone(), &src.name) {
            return failed(format!(
                "sources '{prev}' and '{}' share path '{}'",
                src.name,
                normalised.display()
            ));
        }
    }

    // #58: a malformed allow-list entry would fail every NetworkPolicy apply,
    // or silently block the destination the user meant to allow.
    for rule in spec.egress.iter().flat_map(|e| &e.allow) {
        if let Some(problem) = cidr_problem(&rule.cidr) {
            return failed(format!("egress cidr '{}': {problem}", rule.cidr));
        }
        if let Some(port) = rule.ports.iter().find(|p| !(1..=65535).contains(*p)) {
            return failed(format!(
                "egress cidr '{}': port {port} is outside 1-65535",
                rule.cidr
            ));
        }
    }

    let mut seen = std::collections::BTreeSet::new();
    for p in &spec.providers {
        if !seen.insert(p.name.as_str()) {
            return failed(format!("duplicate provider name '{}'", p.name));
        }
    }
    if let Some(dp) = &spec.default_provider {
        if !spec.providers.iter().any(|p| &p.name == dp) {
            return failed(format!("defaultProvider '{dp}' names no provider"));
        }
    }
    for p in &spec.providers {
        if let Some(c) = &p.credentials_ref {
            if !secret_present(&c.secret_name, &c.key) {
                return failed(format!(
                    "provider '{}': secret '{}' key '{}' not found",
                    p.name, c.secret_name, c.key
                ));
            }
        }
    }
    WorkspaceValidation {
        phase: WorkspacePhase::Ready,
        message: None,
    }
}

/// Why an egress CIDR is invalid, if it is: it needs an IPv4 or IPv6 address and
/// a `/prefix` no longer than the address (32 or 128 bits).
fn cidr_problem(cidr: &str) -> Option<&'static str> {
    let Some((addr, prefix)) = cidr.split_once('/') else {
        return Some("missing a /prefix length");
    };
    let Ok(ip) = addr.parse::<std::net::IpAddr>() else {
        return Some("not an IP address");
    };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    match prefix.parse::<u8>() {
        Ok(p) if p <= max => None,
        _ => Some("prefix length out of range"),
    }
}

/// A provider with its workspace context flattened in — the pinned form.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedProvider {
    /// Provider name.
    pub name: String,
    /// Provider kind.
    pub kind: String,
    /// Base URL, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Credential Secret reference, if the provider needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials_ref: Option<CredentialsRef>,
}

/// The workspace config a `CalibanTask` runs against, resolved to a single
/// provider and pinned into `CalibanTaskStatus.resolvedWorkspace` at admission.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedWorkspace {
    /// The workspace's source checkouts.
    pub sources: Vec<Source>,
    /// The single provider this task binds to.
    pub provider: ResolvedProvider,
    /// Non-secret env from the workspace.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<EnvEntry>,
    /// Workspace default isolation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub isolation: Option<IsolationSpec>,
    /// Workspace egress restriction, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress: Option<EgressSpec>,
    /// Workspace agent policy, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_policy: Option<AgentPolicy>,
}

impl ResolvedWorkspace {
    /// Whether the pinned policy admits an `unattended` task. A pin without a
    /// policy (including one made before the field existed) does not.
    pub fn allows_unattended(&self) -> bool {
        self.agent_policy
            .as_ref()
            .is_some_and(|p| p.allow_unattended)
    }
}

/// Resolve a `WorkspaceSpec` + optional `providerRef` to a single-provider
/// `ResolvedWorkspace`. Provider selection: explicit `provider_ref` →
/// `defaultProvider` → the sole provider if there's exactly one. Errors on a
/// dangling ref or an ambiguous choice.
pub fn resolve_workspace(
    spec: &WorkspaceSpec,
    provider_ref: Option<&str>,
) -> Result<ResolvedWorkspace, String> {
    let chosen = match provider_ref {
        Some(name) => spec
            .providers
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| format!("providerRef '{name}' names no provider in the workspace"))?,
        None => match &spec.default_provider {
            Some(dp) => spec
                .providers
                .iter()
                .find(|p| &p.name == dp)
                .ok_or_else(|| format!("defaultProvider '{dp}' names no provider"))?,
            None if spec.providers.len() == 1 => &spec.providers[0],
            None => {
                return Err(format!(
                    "no providerRef and workspace has no defaultProvider among {} providers",
                    spec.providers.len()
                ))
            }
        },
    };
    Ok(ResolvedWorkspace {
        sources: spec.sources.clone(),
        provider: ResolvedProvider {
            name: chosen.name.clone(),
            kind: chosen.kind.clone(),
            base_url: chosen.base_url.clone(),
            model: chosen.model.clone(),
            credentials_ref: chosen.credentials_ref.clone(),
        },
        env: spec.env.clone(),
        isolation: spec.isolation.clone(),
        egress: spec.egress.clone(),
        agent_policy: spec.agent_policy.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    fn spec_with(providers: Vec<Provider>, default_provider: Option<&str>) -> WorkspaceSpec {
        WorkspaceSpec {
            display_name: "Team A".into(),
            sources: vec![Source {
                name: "caliban".into(),
                repo: "git@x:caliban".into(),
                r#ref: "main".into(),
                path: "/work/caliban".into(),
            }],
            providers,
            default_provider: default_provider.map(String::from),
            env: vec![],
            isolation: None,
            egress: None,
            agent_policy: None,
        }
    }

    fn provider(name: &str, cred: Option<(&str, &str)>) -> Provider {
        Provider {
            name: name.into(),
            kind: "anthropic".into(),
            base_url: None,
            model: None,
            credentials_ref: cred.map(|(s, k)| CredentialsRef {
                secret_name: s.into(),
                key: k.into(),
            }),
        }
    }

    #[test]
    fn valid_workspace_is_ready() {
        let spec = spec_with(
            vec![provider("planner", Some(("anthropic-key", "api-key")))],
            Some("planner"),
        );
        let v = validate_workspace(&spec, "/work", |s, k| {
            s == "anthropic-key" && k == "api-key"
        });
        assert_eq!(v.phase, WorkspacePhase::Ready);
        assert!(v.message.is_none());
    }

    fn egress(cidr: &str, ports: &[i32]) -> EgressSpec {
        EgressSpec {
            allow: vec![EgressRule {
                cidr: cidr.into(),
                ports: ports.to_vec(),
            }],
        }
    }

    /// #58: the allow-list is workspace-wide and pinned at admission, like the
    /// rest of the workspace config a task runs against.
    #[test]
    fn resolve_pins_the_workspace_egress_allow_list() {
        let mut spec = spec_with(vec![provider("workers", None)], None);
        spec.egress = Some(egress("10.0.0.0/8", &[443]));
        let rw = resolve_workspace(&spec, None).unwrap();
        assert_eq!(rw.egress.unwrap().allow[0].cidr, "10.0.0.0/8");
    }

    #[test]
    fn a_well_formed_egress_allow_list_is_ready() {
        let mut spec = spec_with(vec![provider("workers", None)], None);
        spec.egress = Some(EgressSpec {
            allow: vec![
                EgressRule {
                    cidr: "10.0.0.0/8".into(),
                    ports: vec![443],
                },
                EgressRule {
                    cidr: "2001:db8::/32".into(),
                    ports: vec![],
                },
            ],
        });
        let v = validate_workspace(&spec, "/work", |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Ready, "{:?}", v.message);
    }

    /// A malformed rule would otherwise reach the NetworkPolicy and be rejected
    /// on every apply, or silently block the destination the user meant to allow.
    #[test]
    fn a_malformed_egress_cidr_fails_naming_it() {
        for bad in [
            "10.0.0.0",
            "300.1.1.1/8",
            "10.0.0.0/33",
            "2001:db8::/129",
            "example.com/32",
        ] {
            let mut spec = spec_with(vec![provider("workers", None)], None);
            spec.egress = Some(egress(bad, &[]));
            let v = validate_workspace(&spec, "/work", |_, _| true);
            assert_eq!(v.phase, WorkspacePhase::Failed, "{bad}");
            assert!(v.message.unwrap().contains(bad), "{bad}");
        }
    }

    #[test]
    fn an_out_of_range_egress_port_fails() {
        for bad in [0, 70_000] {
            let mut spec = spec_with(vec![provider("workers", None)], None);
            spec.egress = Some(egress("10.0.0.0/8", &[bad]));
            let v = validate_workspace(&spec, "/work", |_, _| true);
            assert_eq!(v.phase, WorkspacePhase::Failed, "{bad}");
        }
    }

    #[test]
    fn keyless_provider_needs_no_secret() {
        let mut p = provider("workers", None);
        p.kind = "openai".into();
        let spec = spec_with(vec![p], None);
        let v = validate_workspace(&spec, "/work", |_, _| false); // no secrets exist at all
        assert_eq!(v.phase, WorkspacePhase::Ready);
    }

    #[test]
    fn missing_secret_fails_with_message() {
        let spec = spec_with(
            vec![provider("planner", Some(("anthropic-key", "api-key")))],
            None,
        );
        let v = validate_workspace(&spec, "/work", |_, _| false);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("provider 'planner': secret 'anthropic-key' key 'api-key' not found")
        );
    }

    #[test]
    fn duplicate_provider_names_fail() {
        let spec = spec_with(
            vec![provider("planner", None), provider("planner", None)],
            None,
        );
        let v = validate_workspace(&spec, "/work", |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("duplicate provider name 'planner'")
        );
    }

    #[test]
    fn dangling_default_provider_fails() {
        let spec = spec_with(vec![provider("planner", None)], Some("nope"));
        let v = validate_workspace(&spec, "/work", |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("defaultProvider 'nope' names no provider")
        );
    }

    /// A spec whose sources sit at `paths` (named `s0`, `s1`, …) with one
    /// keyless provider, so only source-path validation can fail.
    fn spec_with_paths(paths: &[&str]) -> WorkspaceSpec {
        let mut spec = spec_with(vec![provider("workers", None)], None);
        spec.sources = paths
            .iter()
            .enumerate()
            .map(|(i, p)| Source {
                name: format!("s{i}"),
                repo: "git@x:r".into(),
                r#ref: "main".into(),
                path: (*p).into(),
            })
            .collect();
        spec
    }

    fn validate_paths(root: &str, paths: &[&str]) -> WorkspaceValidation {
        validate_workspace(&spec_with_paths(paths), root, |_, _| true)
    }

    // #46: the clone init container mounts the PVC at the workspace root; a
    // source outside it clones into the container's ephemeral layer and is
    // silently lost (and re-cloned) on every restart.

    #[test]
    fn sources_under_the_workspace_root_are_ready() {
        let v = validate_paths("/work", &["/work/caliban", "/work/team/app"]);
        assert_eq!(v.phase, WorkspacePhase::Ready);
        assert!(v.message.is_none());
    }

    #[test]
    fn a_trailing_slash_on_the_root_is_tolerated() {
        let v = validate_paths("/work/", &["/work/caliban"]);
        assert_eq!(v.phase, WorkspacePhase::Ready);
    }

    #[test]
    fn a_source_outside_the_root_fails_naming_the_source() {
        let v = validate_paths("/work", &["/srv/app"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some(
                "source 's0': path '/srv/app' must be a directory under the workspace root '/work'"
            )
        );
    }

    #[test]
    fn a_sibling_sharing_the_root_as_a_string_prefix_fails() {
        // `/workspace/x` starts with the string `/work` but is not under it.
        let v = validate_paths("/work", &["/workspace/x"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
    }

    #[test]
    fn a_source_at_the_root_itself_fails() {
        // Cloning into the mount root would collide with every other source.
        let v = validate_paths("/work", &["/work"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        let v = validate_paths("/work", &["/work/"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
    }

    #[test]
    fn a_parent_segment_cannot_escape_the_root() {
        let v = validate_paths("/work", &["/work/../etc"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        let v = validate_paths("/work", &["/work/a/../../etc"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
    }

    #[test]
    fn a_relative_source_path_fails() {
        let v = validate_paths("/work", &["work/caliban"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
    }

    #[test]
    fn two_sources_sharing_a_path_fail() {
        // The second clone is skipped by the `.git` guard, silently.
        let v = validate_paths("/work", &["/work/app", "/work/app/"]);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("sources 's0' and 's1' share path '/work/app'")
        );
    }

    #[test]
    fn crd_has_correct_group_version_kind() {
        let crd = Workspace::crd();
        assert_eq!(crd.spec.group, "caliban.caliban-ai.dev");
        assert_eq!(crd.spec.names.kind, "Workspace");
        assert_eq!(crd.spec.versions[0].name, "v1alpha1");
        assert_eq!(crd.spec.scope, "Namespaced");
        assert!(crd.spec.versions[0]
            .subresources
            .as_ref()
            .unwrap()
            .status
            .is_some());
    }

    #[test]
    fn crd_enforces_non_empty_required_fields() {
        let crd = Workspace::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let spec = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"];
        assert_eq!(spec["providers"]["minItems"], 1);
        assert_eq!(spec["displayName"]["minLength"], 1);
        let prov = &spec["providers"]["items"]["properties"];
        assert_eq!(prov["name"]["minLength"], 1);
        assert_eq!(prov["kind"]["minLength"], 1);
    }

    /// #21: a workspace may legitimately start with no checkouts and have them
    /// added later, so `minItems: 1` + `required` on `sources` wrongly blocked
    /// registering one at all (422 from the apiserver). `providers` keeps its
    /// constraint — `resolve` relies on there being a bindable provider.
    #[test]
    fn crd_agent_policy_allow_unattended_is_an_optional_boolean_defaulting_false() {
        // #80: an omitted policy, or one without the switch, must never read
        // as permission to run unattended.
        let crd = Workspace::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let policy = &schema["openAPIV3Schema"]["properties"]["spec"]["properties"]["agentPolicy"];
        let allow = &policy["properties"]["allowUnattended"];
        assert_eq!(allow["type"], "boolean", "{policy}");
        assert_eq!(allow["default"], false, "{policy}");

        let spec: WorkspaceSpec = serde_json::from_value(serde_json::json!({
            "displayName": "x",
            "providers": [{ "name": "p", "kind": "openai" }],
            "agentPolicy": {}
        }))
        .unwrap();
        assert_eq!(spec.agent_policy, Some(AgentPolicy::default()));
        let rw = resolve_workspace(&spec, None).unwrap();
        assert!(!rw.allows_unattended());
    }

    #[test]
    fn crd_allows_a_workspace_with_no_sources() {
        let crd = Workspace::crd();
        let schema = serde_json::to_value(&crd.spec.versions[0].schema).unwrap();
        let spec = &schema["openAPIV3Schema"]["properties"]["spec"];

        assert!(
            spec["properties"]["sources"]["minItems"].is_null(),
            "sources must not carry minItems"
        );
        let required: Vec<&str> = spec["required"]
            .as_array()
            .expect("spec.required")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            !required.contains(&"sources"),
            "sources must not be required, got {required:?}"
        );
        // The tightening this ticket does NOT relax.
        assert!(required.contains(&"providers"));
        assert!(required.contains(&"displayName"));
    }

    /// The Rust type must accept the same source-less spec the schema now does.
    #[test]
    fn source_less_workspace_deserializes() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: Workspace
metadata: { name: bare, namespace: team-a }
spec:
  displayName: Bare
  providers:
    - { name: only, kind: openai, baseUrl: "http://localhost:9292/v1" }
"#;
        let ws: Workspace = serde_norway::from_str(yaml).unwrap();
        assert!(ws.spec.sources.is_empty());
        assert_eq!(ws.spec.providers.len(), 1);
    }

    #[test]
    fn sample_cr_round_trips() {
        let yaml = r#"
apiVersion: caliban.caliban-ai.dev/v1alpha1
kind: Workspace
metadata: { name: team-a-ws, namespace: team-a }
spec:
  displayName: Team A
  sources:
    - { name: caliban, repo: "git@example:caliban", ref: main, path: /work/caliban }
  providers:
    - { name: planner, kind: anthropic, model: claude-opus-4-8, credentialsRef: { secretName: anthropic-key, key: api-key } }
    - { name: workers, kind: openai, baseUrl: "http://localhost:9292/v1", model: qwen2.5-coder }
  defaultProvider: planner
"#;
        let ws: Workspace = serde_norway::from_str(yaml).unwrap();
        assert_eq!(ws.spec.providers.len(), 2);
        assert_eq!(ws.spec.providers[0].name, "planner");
        assert_eq!(
            ws.spec.providers[0]
                .credentials_ref
                .as_ref()
                .unwrap()
                .secret_name,
            "anthropic-key"
        );
        assert!(ws.spec.providers[1].credentials_ref.is_none());
        assert_eq!(ws.spec.default_provider.as_deref(), Some("planner"));
        // camelCase survives round-trip.
        let v = serde_json::to_value(&ws.spec).unwrap();
        assert!(v["displayName"].is_string());
    }

    #[test]
    fn committed_crd_yaml_is_in_sync() {
        let generated = serde_norway::to_string(&Workspace::crd()).unwrap();
        let committed = include_str!("../deploy/crd/workspace.yaml");
        assert_eq!(
            generated.trim(),
            committed.trim(),
            "deploy/crd/workspace.yaml is stale — regenerate: cargo run --bin crdgen workspace > deploy/crd/workspace.yaml"
        );
    }

    #[test]
    fn resolve_picks_named_provider() {
        let spec = spec_with(
            vec![
                provider("planner", Some(("k", "v"))),
                provider("workers", None),
            ],
            Some("planner"),
        );
        let r = resolve_workspace(&spec, Some("workers")).unwrap();
        assert_eq!(r.provider.name, "workers");
        assert_eq!(r.sources.len(), 1);
    }

    #[test]
    fn resolve_falls_back_to_default_provider() {
        let spec = spec_with(
            vec![provider("planner", None), provider("workers", None)],
            Some("workers"),
        );
        let r = resolve_workspace(&spec, None).unwrap();
        assert_eq!(r.provider.name, "workers");
    }

    #[test]
    fn resolve_uses_sole_provider_when_no_default() {
        let spec = spec_with(vec![provider("only", None)], None);
        let r = resolve_workspace(&spec, None).unwrap();
        assert_eq!(r.provider.name, "only");
    }

    #[test]
    fn resolve_ambiguous_without_default_errors() {
        let spec = spec_with(vec![provider("a", None), provider("b", None)], None);
        let err = resolve_workspace(&spec, None).unwrap_err();
        assert_eq!(
            err,
            "no providerRef and workspace has no defaultProvider among 2 providers"
        );
    }

    #[test]
    fn resolve_dangling_provider_ref_errors() {
        let spec = spec_with(vec![provider("planner", None)], None);
        let err = resolve_workspace(&spec, Some("nope")).unwrap_err();
        assert_eq!(err, "providerRef 'nope' names no provider in the workspace");
    }

    #[test]
    fn sample_manifests_deserialize() {
        let ws: Workspace =
            serde_norway::from_str(include_str!("../deploy/samples/workspace.yaml")).unwrap();
        assert_eq!(ws.spec.providers.len(), 2);
        let ct: crate::crd::CalibanTask =
            serde_norway::from_str(include_str!("../deploy/samples/calibantask.yaml")).unwrap();
        assert_eq!(ct.spec.workspace_ref.name, "team-a-ws");
        assert_eq!(ct.spec.provider_ref.as_deref(), Some("workers"));
        // The sample CalibanTask references a provider the sample Workspace defines.
        let r = resolve_workspace(&ws.spec, ct.spec.provider_ref.as_deref()).unwrap();
        assert_eq!(r.provider.kind, "openai");
    }
}
