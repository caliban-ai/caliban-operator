//! How a `CalibanTask`'s `spec.state` reaches caliban (#41, #53): caliban's
//! storage settings, set through the environment overrides caliban#659 added.
//!
//! caliban reaches a remote gonzalod only through `storage.substrate`,
//! `storage.remote.url` and `storage.remote.token_env`; nothing reads
//! `GONZALO_ENDPOINT`. Setting those settings by environment keeps the operator
//! from authoring caliban's settings file (ADR 0005).

use k8s_openapi::api::core::v1::{EnvVar, EnvVarSource, Secret, SecretKeySelector};

use crate::crd::StateSpec;
use crate::workspace::CredentialsRef;

/// caliban#659: overrides `storage.substrate` (`fs` / `remote`).
pub const ENV_SUBSTRATE: &str = "CALIBAN_STORAGE_SUBSTRATE";
/// caliban#659: overrides `storage.remote.url`.
pub const ENV_REMOTE_URL: &str = "CALIBAN_STORAGE_REMOTE_URL";
/// caliban#659: overrides `storage.remote.token_env`, the *name* of the variable
/// holding the bearer token.
pub const ENV_REMOTE_TOKEN_ENV: &str = "CALIBAN_STORAGE_REMOTE_TOKEN_ENV";
/// The variable the gonzalod bearer token is projected into from its Secret.
pub const TOKEN_ENV: &str = "GONZALO_TOKEN";

/// caliban's storage substrate for a `spec.state`, in caliban's vocabulary. The
/// CRD's `local` is caliban's `fs`. An unset mode with an endpoint means remote.
fn substrate(state: &StateSpec) -> Option<&'static str> {
    match state.mode.as_deref() {
        Some("remote") => Some("remote"),
        Some("local") => Some("fs"),
        Some(_) => None, // rejected by `state_problem`
        None => state.gonzalo_endpoint.as_ref().map(|_| "remote"),
    }
}

/// Why a `spec.state` can't be projected, if it can't. Each case would otherwise
/// fail late or silently: an unknown mode, remote storage caliban refuses to
/// start without a URL, an endpoint local mode never uses, or a token with no
/// remote store to authenticate to.
pub fn state_problem(state: &StateSpec) -> Option<String> {
    match state.mode.as_deref() {
        None | Some("remote") | Some("local") => {}
        Some(other) => return Some(format!("state.mode '{other}' is not one of: remote, local")),
    }
    if state.mode.as_deref() == Some("remote") && state.gonzalo_endpoint.is_none() {
        return Some("state.mode remote requires state.gonzaloEndpoint".to_string());
    }
    if state.mode.as_deref() == Some("local") && state.gonzalo_endpoint.is_some() {
        return Some(
            "state.gonzaloEndpoint is set, but state.mode local never uses it".to_string(),
        );
    }
    if state.token_ref.is_some() && substrate(state) != Some("remote") {
        return Some(
            "state.tokenRef requires remote storage (set state.gonzaloEndpoint)".to_string(),
        );
    }
    None
}

/// caliband environment for a task's `spec.state`. Empty when there is no
/// state, so a task without one gets exactly the pod spec it had before #41.
pub fn storage_env(state: Option<&StateSpec>) -> Vec<EnvVar> {
    let Some(state) = state else {
        return Vec::new();
    };
    let Some(substrate) = substrate(state) else {
        return Vec::new();
    };
    let mut env = vec![plain(ENV_SUBSTRATE, substrate)];
    if substrate == "remote" {
        if let Some(url) = &state.gonzalo_endpoint {
            env.push(plain(ENV_REMOTE_URL, url));
        }
        if let Some(r) = &state.token_ref {
            // Only the token variable's *name* is a setting; the token itself
            // comes from the Secret and never sits in the pod spec.
            env.push(plain(ENV_REMOTE_TOKEN_ENV, TOKEN_ENV));
            env.push(EnvVar {
                name: TOKEN_ENV.to_string(),
                value: None,
                value_from: Some(EnvVarSource {
                    secret_key_ref: Some(SecretKeySelector {
                        name: r.secret_name.clone(),
                        key: r.key.clone(),
                        optional: Some(false),
                    }),
                    ..Default::default()
                }),
            });
        }
    }
    env
}

fn plain(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    }
}

/// Why the gonzalod token Secret can't back the task, if it can't: missing, or
/// without the referenced key in `data` or `stringData`.
pub fn token_secret_problem(secret: Option<&Secret>, r: &CredentialsRef) -> Option<String> {
    let Some(secret) = secret else {
        return Some(format!(
            "state.tokenRef: secret '{}' not found",
            r.secret_name
        ));
    };
    let has_key = secret.data.as_ref().is_some_and(|d| d.contains_key(&r.key))
        || secret
            .string_data
            .as_ref()
            .is_some_and(|d| d.contains_key(&r.key));
    (!has_key).then(|| {
        format!(
            "state.tokenRef: secret '{}' has no key '{}'",
            r.secret_name, r.key
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::StateSpec;
    use crate::workspace::CredentialsRef;
    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::ByteString;
    use std::collections::BTreeMap;

    fn state(endpoint: Option<&str>, mode: Option<&str>, token: Option<(&str, &str)>) -> StateSpec {
        StateSpec {
            gonzalo_endpoint: endpoint.map(String::from),
            mode: mode.map(String::from),
            token_ref: token.map(|(s, k)| CredentialsRef {
                secret_name: s.into(),
                key: k.into(),
            }),
        }
    }

    fn value<'a>(env: &'a [EnvVar], name: &str) -> Option<&'a str> {
        env.iter()
            .find(|e| e.name == name)
            .and_then(|e| e.value.as_deref())
    }

    /// No `state` means the pod spec is exactly what it was before #41.
    #[test]
    fn no_state_projects_nothing() {
        assert!(storage_env(None).is_empty());
    }

    #[test]
    fn an_endpoint_selects_remote_storage_at_that_url() {
        let env = storage_env(Some(&state(
            Some("http://gonzalod.storage.svc:8080"),
            None,
            None,
        )));
        assert_eq!(value(&env, "CALIBAN_STORAGE_SUBSTRATE"), Some("remote"));
        assert_eq!(
            value(&env, "CALIBAN_STORAGE_REMOTE_URL"),
            Some("http://gonzalod.storage.svc:8080")
        );
        assert!(env
            .iter()
            .all(|e| e.name != "CALIBAN_STORAGE_REMOTE_TOKEN_ENV" && e.name != "GONZALO_TOKEN"));
    }

    /// caliban never read `GONZALO_ENDPOINT` (#41/#53); it must not come back.
    #[test]
    fn the_inert_gonzalo_endpoint_variable_is_gone() {
        let env = storage_env(Some(&state(
            Some("http://g:8080"),
            Some("remote"),
            Some(("gz", "token")),
        )));
        assert!(env.iter().all(|e| e.name != "GONZALO_ENDPOINT"));
    }

    /// The CRD says `local`; caliban's vocabulary for the same thing is `fs`.
    #[test]
    fn local_mode_selects_the_fs_substrate() {
        let env = storage_env(Some(&state(None, Some("local"), None)));
        assert_eq!(value(&env, "CALIBAN_STORAGE_SUBSTRATE"), Some("fs"));
        assert!(env.iter().all(|e| e.name != "CALIBAN_STORAGE_REMOTE_URL"));
    }

    /// The token itself never sits in the pod spec: only its variable's name
    /// is set, and the variable is filled from the Secret.
    #[test]
    fn a_token_ref_names_the_token_variable_and_projects_it_from_the_secret() {
        let env = storage_env(Some(&state(
            Some("http://g:8080"),
            Some("remote"),
            Some(("gonzalo-agent", "token")),
        )));
        assert_eq!(
            value(&env, "CALIBAN_STORAGE_REMOTE_TOKEN_ENV"),
            Some("GONZALO_TOKEN")
        );
        let token = env
            .iter()
            .find(|e| e.name == "GONZALO_TOKEN")
            .expect("token variable");
        assert!(token.value.is_none(), "the token must never be inlined");
        let selector = token
            .value_from
            .as_ref()
            .and_then(|v| v.secret_key_ref.as_ref())
            .expect("secretKeyRef");
        assert_eq!(selector.name, "gonzalo-agent");
        assert_eq!(selector.key, "token");
        assert_eq!(selector.optional, Some(false));
    }

    #[test]
    fn a_well_formed_state_has_no_problem() {
        assert!(state_problem(&state(Some("http://g:8080"), None, Some(("s", "k")))).is_none());
        assert!(state_problem(&state(Some("http://g:8080"), Some("remote"), None)).is_none());
        assert!(state_problem(&state(None, Some("local"), None)).is_none());
        assert!(state_problem(&state(None, None, None)).is_none());
    }

    #[test]
    fn an_unknown_mode_is_a_problem_naming_it() {
        let p = state_problem(&state(Some("http://g:8080"), Some("s3"), None)).unwrap();
        assert!(p.contains("s3"), "{p}");
    }

    /// caliban refuses to start with remote storage and no URL. Failing the
    /// task says why, instead of a crash-looping pod.
    #[test]
    fn remote_mode_without_an_endpoint_is_a_problem() {
        assert!(state_problem(&state(None, Some("remote"), None)).is_some());
    }

    /// An endpoint that `mode: local` would silently ignore.
    #[test]
    fn an_endpoint_with_local_mode_is_a_problem() {
        assert!(state_problem(&state(Some("http://g:8080"), Some("local"), None)).is_some());
    }

    /// A token with no remote store to authenticate to would do nothing.
    #[test]
    fn a_token_ref_without_remote_storage_is_a_problem() {
        assert!(state_problem(&state(None, Some("local"), Some(("s", "k")))).is_some());
        assert!(state_problem(&state(None, None, Some(("s", "k")))).is_some());
    }

    #[test]
    fn a_missing_token_secret_or_key_is_a_problem() {
        let r = CredentialsRef {
            secret_name: "gonzalo-agent".into(),
            key: "token".into(),
        };
        let p = token_secret_problem(None, &r).expect("missing secret");
        assert!(p.contains("gonzalo-agent"), "{p}");

        let wrong_key = Secret {
            data: Some(BTreeMap::from([(
                "other".to_string(),
                ByteString(b"x".to_vec()),
            )])),
            ..Default::default()
        };
        let p = token_secret_problem(Some(&wrong_key), &r).expect("missing key");
        assert!(p.contains("token"), "{p}");

        let in_data = Secret {
            data: Some(BTreeMap::from([(
                "token".to_string(),
                ByteString(b"x".to_vec()),
            )])),
            ..Default::default()
        };
        assert!(token_secret_problem(Some(&in_data), &r).is_none());

        let in_string_data = Secret {
            string_data: Some(BTreeMap::from([("token".to_string(), "x".to_string())])),
            ..Default::default()
        };
        assert!(token_secret_problem(Some(&in_string_data), &r).is_none());
    }
}
