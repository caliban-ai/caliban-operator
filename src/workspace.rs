//! The `Workspace` custom resource (v1alpha1): durable, shared config — sources,
//! named providers (each with its own model + Secret reference), env, isolation —
//! that `CalibanTask`s reference by name. Owned and reconciled by the operator,
//! which is the sole reader of provider credential Secrets. See ADR 0004 and the
//! Prospero K8s Config Plane design.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::crd::{Condition, IsolationSpec, Source};

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
    /// The workspace's git checkouts (1..N).
    #[schemars(length(min = 1))]
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
}

/// A named model provider bound within a workspace.
#[derive(Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// Provider identifier, unique within the workspace (e.g. `planner`).
    #[schemars(length(min = 1))]
    pub name: String,
    /// Provider kind (e.g. `ollama`, `anthropic`, `openai`).
    #[schemars(length(min = 1))]
    pub kind: String,
    /// Override base URL (e.g. `http://192.168.1.240:11434`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Default model for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reference to an existing Secret for this provider's API key. Keyless
    /// providers (e.g. ollama) omit it.
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

/// Pure validation of a `WorkspaceSpec`: unique provider names, a resolvable
/// `defaultProvider`, and an existing Secret key for every `credentialsRef`.
/// `secret_present(secret_name, key)` reports Secret-key existence (cluster
/// lookup is the caller's responsibility). First problem found wins.
pub fn validate_workspace(
    spec: &WorkspaceSpec,
    secret_present: impl Fn(&str, &str) -> bool,
) -> WorkspaceValidation {
    fn failed(message: String) -> WorkspaceValidation {
        WorkspaceValidation {
            phase: WorkspacePhase::Failed,
            message: Some(message),
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
        let v = validate_workspace(&spec, |s, k| s == "anthropic-key" && k == "api-key");
        assert_eq!(v.phase, WorkspacePhase::Ready);
        assert!(v.message.is_none());
    }

    #[test]
    fn keyless_provider_needs_no_secret() {
        let mut p = provider("workers", None);
        p.kind = "ollama".into();
        let spec = spec_with(vec![p], None);
        let v = validate_workspace(&spec, |_, _| false); // no secrets exist at all
        assert_eq!(v.phase, WorkspacePhase::Ready);
    }

    #[test]
    fn missing_secret_fails_with_message() {
        let spec = spec_with(
            vec![provider("planner", Some(("anthropic-key", "api-key")))],
            None,
        );
        let v = validate_workspace(&spec, |_, _| false);
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
        let v = validate_workspace(&spec, |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("duplicate provider name 'planner'")
        );
    }

    #[test]
    fn dangling_default_provider_fails() {
        let spec = spec_with(vec![provider("planner", None)], Some("nope"));
        let v = validate_workspace(&spec, |_, _| true);
        assert_eq!(v.phase, WorkspacePhase::Failed);
        assert_eq!(
            v.message.as_deref(),
            Some("defaultProvider 'nope' names no provider")
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
        assert_eq!(spec["sources"]["minItems"], 1);
        assert_eq!(spec["providers"]["minItems"], 1);
        assert_eq!(spec["displayName"]["minLength"], 1);
        let prov = &spec["providers"]["items"]["properties"];
        assert_eq!(prov["name"]["minLength"], 1);
        assert_eq!(prov["kind"]["minLength"], 1);
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
    - { name: workers, kind: ollama, baseUrl: "http://192.168.1.240:11434", model: qwen2.5-coder }
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
        assert_eq!(r.provider.kind, "ollama");
    }
}
