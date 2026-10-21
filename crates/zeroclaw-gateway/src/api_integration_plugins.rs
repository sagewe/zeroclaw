//! Declarative service packages. Installation supplies files; activation is an authorized config commit.
use crate::{
    AppState,
    api::require_auth,
    principal_gate::{ConfigWriteSet, RequestPrincipal, authorize_config_write},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroclaw_config::{
    integration_plugins::{
        IntegrationManifest, IntegrationPluginsConfig, load_package, package_root,
    },
    schema::McpTransport,
};
#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IntegrationPackageView {
    pub package: String,
    pub manifest: Option<IntegrationManifest>,
    pub sha256: Option<String>,
    pub error: Option<String>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IntegrationConnection {
    pub name: String,
    pub packages: Vec<String>,
}
#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IntegrationPluginsResponse {
    pub config: IntegrationPluginsConfig,
    pub revision: String,
    pub packages: Vec<IntegrationPackageView>,
    pub connections: Vec<IntegrationConnection>,
    pub invalid_instances: Vec<String>,
    pub discovery_error: bool,
    pub push_credentials_configured: Vec<String>,
}
#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct IntegrationPluginsWrite {
    pub config: IntegrationPluginsConfig,
    pub revision: String,
}
fn revision(config: &IntegrationPluginsConfig) -> anyhow::Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(
        &serde_json::to_value(config)?,
    )?)))
}
fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({"error":message.to_string()}))).into_response()
}
pub async fn get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let root = package_root(&config);
    let mut packages = Vec::new();
    let mut discovery_error = false;
    match std::fs::read_dir(&root) {
        Ok(entries) => {
            for entry in entries.take(128) {
                let Ok(entry) = entry else {
                    discovery_error = true;
                    continue;
                };
                let name = entry.file_name().to_string_lossy().into_owned();
                if !zeroclaw_config::events::valid_id(&name) {
                    continue;
                }
                match load_package(&root, &name) {
                    Ok((manifest, digest)) => packages.push(IntegrationPackageView {
                        package: name,
                        manifest: Some(manifest),
                        sha256: Some(digest),
                        error: None,
                    }),
                    Err(_) => packages.push(IntegrationPackageView {
                        package: name,
                        manifest: None,
                        sha256: None,
                        error: Some("invalid_package".into()),
                    }),
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => discovery_error = true,
    }
    packages.sort_by(|a, b| a.package.cmp(&b.package));
    let invalid_instances = config
        .integration_plugins
        .instances
        .keys()
        .filter(|id| config.integration_manifest(id).is_err())
        .cloned()
        .collect();
    let connections = config
        .mcp
        .servers
        .iter()
        .filter(|s| s.transport == McpTransport::Http)
        .map(|s| IntegrationConnection {
            name: s.name.clone(),
            packages: packages
                .iter()
                .filter(|p| {
                    p.manifest.as_ref().is_some_and(|m| {
                        m.mcp.as_ref().is_some_and(|m| m.matches(s.url.as_deref()))
                    })
                })
                .map(|p| p.package.clone())
                .collect(),
        })
        .collect();
    let rev = match revision(&config.integration_plugins) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut public_config = config.integration_plugins;
    let mut push_credentials_configured = Vec::new();
    for (id, binding) in &mut public_config.instances {
        if binding.push_token.take().is_some_and(|s| !s.is_empty()) {
            push_credentials_configured.push(id.clone());
        }
    }
    Json(IntegrationPluginsResponse {
        config: public_config,
        push_credentials_configured,
        revision: rev,
        packages,
        connections,
        invalid_instances,
        discovery_error,
    })
    .into_response()
}
pub async fn put(
    State(state): State<AppState>,
    principal: RequestPrincipal,
    Json(body): Json<IntegrationPluginsWrite>,
) -> Response {
    let guard = match state.begin_config_commit().await {
        Ok(g) => g,
        Err(e) => return error(StatusCode::CONFLICT, e),
    };
    let before = guard.current_config();
    if revision(&before.integration_plugins).ok().as_deref() != Some(&body.revision) {
        return error(
            StatusCode::CONFLICT,
            "Integration settings changed; refresh before saving",
        );
    }
    let mut working = before.clone();
    working.integration_plugins = body.config;
    for (id, binding) in &mut working.integration_plugins.instances {
        // Omitted/null input keeps the credential for the same service/account binding.
        // An explicit empty string clears it. GET never returns a secret or a mask to resubmit.
        if binding.push_token.is_none() {
            binding.push_token = before
                .integration_plugins
                .instances
                .get(id)
                .filter(|old| {
                    old.package == binding.package && old.mcp_server == binding.mcp_server
                })
                .and_then(|old| old.push_token.clone());
        } else if binding
            .push_token
            .as_ref()
            .is_some_and(|s| s.trim().is_empty())
        {
            binding.push_token = None;
        }
    }
    if let Err(e) = working.validate() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, e);
    }
    for (id, binding) in &working.integration_plugins.instances {
        if binding.enabled {
            if binding.push_enabled {
                if let Err(e) = working.integration_push(id) {
                    return error(StatusCode::UNPROCESSABLE_ENTITY, e);
                }
            }
            match working.integration_manifest(id) {
                Ok(manifest) if !binding.push_enabled || manifest.push.is_some() => {}
                Ok(_) => {
                    return error(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "Package does not declare push",
                    );
                }
                Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e),
            }
        }
    }
    let mut paths = Vec::new();
    for id in before
        .integration_plugins
        .instances
        .keys()
        .chain(working.integration_plugins.instances.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if before.integration_plugins.instances.get(id)
            != working.integration_plugins.instances.get(id)
        {
            paths.push(format!("integration_plugins.instances.{id}"));
        }
    }
    let auth = match authorize_config_write(
        &principal,
        ConfigWriteSet::by_effect(&before, &working, paths.iter().map(String::as_str)),
        &guard,
    ) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    for path in paths {
        working.mark_dirty(&path);
    }
    if let Err(e) = crate::api_config::persist_and_swap(&state, auth, working, guard).await {
        return e;
    }
    Json(json!({"saved":true,"reload_scheduled":true})).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn integration_catalog_and_mutation_require_authentication() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, true, false);
        assert_eq!(
            get(State(state.clone()), HeaderMap::new()).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let auth = crate::principal_gate::GatewayInboundAuth::from_config(
            &state.config.read(),
            state.pairing.clone(),
        )
        .unwrap();
        let router = crate::config_admin_router(&std::sync::Arc::new(auth)).with_state(state);
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/integration-plugins")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"config":{"instances":{}},"revision":"stale"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn catalog_redacts_push_token_and_unrelated_edits_preserve_it() {
        use axum::body::to_bytes;
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, false, false);
        let mut config = state.config.read().clone();
        config.integration_plugins.instances.insert(
            "mail".into(),
            zeroclaw_config::integration_plugins::IntegrationBinding {
                package: "fastmail".into(),
                mcp_server: "mail".into(),
                manifest_sha256: "0".repeat(64),
                push_token: Some("private-push-fixture".into()),
                ..Default::default()
            },
        );
        state.config_authority.publish_for_test(config);
        let response = get(State(state.clone()), HeaderMap::new()).await;
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let data: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            !String::from_utf8(bytes.to_vec())
                .unwrap()
                .contains("private-push-fixture")
        );
        assert_eq!(data["push_credentials_configured"], json!(["mail"]));
        let incoming: IntegrationPluginsConfig =
            serde_json::from_value(data["config"].clone()).unwrap();
        assert!(incoming.instances["mail"].push_token.is_none());
        let response = put(
            State(state.clone()),
            None,
            Json(IntegrationPluginsWrite {
                config: incoming,
                revision: data["revision"].as_str().unwrap().into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.config.read().integration_plugins.instances["mail"]
                .push_token
                .as_deref(),
            Some("private-push-fixture")
        );
    }

    #[tokio::test]
    async fn stale_or_unpinned_integration_cannot_be_activated() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, false, false);
        let mut config = IntegrationPluginsConfig::default();
        assert_eq!(
            put(
                State(state.clone()),
                None,
                Json(IntegrationPluginsWrite {
                    config: config.clone(),
                    revision: "stale".into()
                })
            )
            .await
            .status(),
            StatusCode::CONFLICT
        );
        let rev = revision(&config).unwrap();
        config.instances.insert(
            "mail".into(),
            zeroclaw_config::integration_plugins::IntegrationBinding {
                enabled: true,
                package: "missing".into(),
                mcp_server: "mail".into(),
                manifest_sha256: "0".repeat(64),
                ..Default::default()
            },
        );
        assert_eq!(
            put(
                State(state.clone()),
                None,
                Json(IntegrationPluginsWrite {
                    config,
                    revision: rev
                })
            )
            .await
            .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert!(state.config.read().integration_plugins.instances.is_empty());
    }
}
