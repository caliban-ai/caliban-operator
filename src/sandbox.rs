//! A minimal foreign view of agent-sandbox's `Sandbox` CRD
//! (`agents.x-k8s.io/v1beta1`). We consume it — we declare only the fields the
//! operator sets or reads; the API server prunes the rest. We never emit this
//! CRD (agent-sandbox owns it), so `Sandbox::crd()` is intentionally unused. See
//! ADR 0002.

use k8s_openapi::api::core::v1::{PersistentVolumeClaimSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
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
    pub volume_claim_templates: Option<Vec<VolumeClaimTemplate>>,
}

/// A `volumeClaimTemplates` entry as agent-sandbox's `Sandbox` schema declares it:
/// a bare embedded PVC template (`metadata` + `spec`) with **no** `apiVersion`/`kind`.
///
/// We must NOT use k8s-openapi's `PersistentVolumeClaim` here: it serializes its
/// group/version/kind, and agent-sandbox v0.5.0's structural schema rejects those on
/// server-side apply (`.spec.volumeClaimTemplates[].apiVersion: field not declared in
/// schema`), which fails every reconcile.
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VolumeClaimTemplate {
    #[serde(default)]
    pub metadata: ObjectMeta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<PersistentVolumeClaimSpec>,
}

/// Observed state of a `Sandbox` (subset the operator reads).
#[derive(Serialize, Deserialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SandboxStatus {
    /// Stable in-cluster DNS name for the sandbox's Service.
    #[serde(
        default,
        rename = "serviceFQDN",
        skip_serializing_if = "Option::is_none"
    )]
    pub service_fqdn: Option<String>,
}

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
    fn volume_claim_templates_carry_no_type_meta() {
        // agent-sandbox v0.5.0's Sandbox schema declares only `metadata`/`spec` on
        // volumeClaimTemplates items; emitting apiVersion/kind fails the SSA with
        // "field not declared in schema". Guard against regressing to a full PVC.
        let sb = Sandbox::new(
            "demo-sbx",
            SandboxSpec {
                pod_template: PodTemplateSpec::default(),
                service: None,
                operating_mode: None,
                volume_claim_templates: Some(vec![VolumeClaimTemplate {
                    metadata: ObjectMeta {
                        name: Some("workspace".to_string()),
                        ..Default::default()
                    },
                    spec: None,
                }]),
            },
        );
        let v = serde_json::to_value(&sb).unwrap();
        let vct = &v["spec"]["volumeClaimTemplates"][0];
        assert!(
            vct.get("apiVersion").is_none(),
            "must not emit apiVersion: {vct}"
        );
        assert!(vct.get("kind").is_none(), "must not emit kind: {vct}");
        assert_eq!(vct["metadata"]["name"], "workspace");
    }

    #[test]
    fn status_reads_service_fqdn() {
        let json = serde_json::json!({ "serviceFQDN": "demo-sbx.team-a.svc" });
        let st: SandboxStatus = serde_json::from_value(json).unwrap();
        assert_eq!(st.service_fqdn.as_deref(), Some("demo-sbx.team-a.svc"));
    }
}
