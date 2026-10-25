//! Private household operator surface. Canonical device state lives in
//! clowder (the home hub since 2026-10-25), read from its `/api/home` next to
//! the MCP endpoint of the `openhab` connection, with that connection's
//! headers. Admission follows the live grants of the same MCP tools: reading
//! needs `get_home_status`, camera frames `get_item`, switching `set_item`
//! (and an autonomy above read-only).
use crate::{AppState, api::require_auth};
use anyhow::{Context, Result, ensure};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroclaw_config::schema::{Config, McpServerConfig};

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct HomeResponse {
    pub agent: String,
    pub can_control: bool,
    pub can_capture: bool,
    pub data: Value,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum HomeAction {
    Refresh {},
    SetPower { light: String, state: PowerState },
    Snapshot {},
    DeviceSpec { device_id: String },
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub enum PowerState {
    On,
    Off,
}

/// The clowder connection keeps openHAB's server name, so the agents'
/// `openhab__*` approvals carry over.
const HOME_SERVER: &str = "openhab";

fn connection(config: &Config, agent: &str, server: &str, tool: &str) -> Result<McpServerConfig> {
    ensure!(
        config.mcp.enabled && config.agents.get(agent).is_some_and(|a| a.enabled),
        "home_disabled"
    );
    ensure!(
        config.integration_poll_allowed(agent, server, tool),
        "home_permission_required"
    );
    config
        .mcp_servers_for_agent(agent)
        .into_iter()
        .find(|s| s.name == server)
        .context("home_connection_not_granted")
}
/// The first enabled agent, in sorted alias order, that may read the home.
fn owner(config: &Config) -> Result<String> {
    let mut agents: Vec<_> = config.agents.keys().collect();
    agents.sort();
    agents
        .into_iter()
        .find(|a| connection(config, a, HOME_SERVER, "get_home_status").is_ok())
        .cloned()
        .context("home_permission_required")
}
fn allowed(config: &Config, agent: &str, tool: &str) -> bool {
    connection(config, agent, HOME_SERVER, tool).is_ok()
        && (tool != "set_item"
            || config
                .risk_profile_for_agent(agent)
                .is_some_and(|r| r.level != zeroclaw_config::autonomy::AutonomyLevel::ReadOnly))
}
/// `/api/home…` on clowder. Admission is resolved again right before the
/// request, from live config.
async fn clowder(
    state: &AppState,
    headers: &HeaderMap,
    agent: &str,
    tool: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let cfg = {
        let config = state.config.read();
        ensure!(owner(&config)? == agent, "home_scope_changed");
        ensure!(allowed(&config, agent, tool), "home_permission_required");
        connection(&config, agent, HOME_SERVER, tool)?
    };
    ensure!(require_auth(state, headers).is_ok(), "home_auth_revoked");
    let url = cfg.url.as_deref().context("home_connection_invalid")?;
    let base = url.trim_end_matches('/').trim_end_matches("/mcp");
    ensure!(base.starts_with("https://"), "home_connection_invalid");
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(50))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut req = match &body {
        Some(b) => http.post(format!("{base}{path}")).json(b),
        None => http.get(format!("{base}{path}")),
    };
    for (k, v) in &cfg.headers {
        req = req.header(k, v);
    }
    let resp = req.send().await?;
    ensure!(resp.status().is_success(), "home_operation_failed");
    let bytes = resp.bytes().await?;
    ensure!(bytes.len() <= 4 * 1024 * 1024, "home_response_invalid");
    Ok(serde_json::from_slice(&bytes)?)
}
fn failure(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}
fn private_json<T: Serialize>(v: T) -> Response {
    ([("cache-control", "no-store")], Json(v)).into_response()
}
pub async fn get_home(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let Ok(agent) = owner(&state.config.read()) else {
        return failure(StatusCode::FORBIDDEN, "home_not_available");
    };
    let Ok(view) = clowder(
        &state,
        &headers,
        &agent,
        "get_home_status",
        "/api/home",
        None,
    )
    .await
    else {
        return failure(StatusCode::BAD_GATEWAY, "home_service_unavailable");
    };
    let config = state.config.read();
    private_json(HomeResponse {
        can_control: allowed(&config, &agent, "set_item"),
        can_capture: allowed(&config, &agent, "get_item")
            && view["cameras"].as_array().is_some_and(|v| !v.is_empty()),
        agent,
        data: view,
    })
}
pub async fn action(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(action): Json<HomeAction>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let Ok(agent) = owner(&state.config.read()) else {
        return failure(StatusCode::FORBIDDEN, "home_not_available");
    };
    let (tool, path, body) = match &action {
        // clowder keeps its device list current itself.
        HomeAction::Refresh {} => ("get_home_status", "/api/home", None),
        HomeAction::SetPower { light, state } => {
            if light.is_empty() || light.len() > 256 {
                return failure(StatusCode::BAD_REQUEST, "home_invalid_light");
            }
            (
                "set_item",
                "/api/home/power",
                Some(
                    json!({"light":light,"state":match state{PowerState::On=>"on",PowerState::Off=>"off"}}),
                ),
            )
        }
        HomeAction::Snapshot {} => ("get_item", "/api/home/snapshot", None),
        // clowder has no MIoT capability definitions to offer.
        HomeAction::DeviceSpec { device_id } => {
            if device_id.is_empty() || device_id.len() > 128 {
                return failure(StatusCode::BAD_REQUEST, "home_invalid_device");
            }
            return private_json(json!({"services": []}));
        }
    };
    if !allowed(&state.config.read(), &agent, tool) {
        return failure(StatusCode::FORBIDDEN, "home_permission_required");
    }
    let Ok(raw) = clowder(&state, &headers, &agent, tool, path, body).await else {
        return failure(
            StatusCode::BAD_GATEWAY,
            "home_operation_unconfirmed_no_retry",
        );
    };
    if matches!(action, HomeAction::Snapshot {})
        && raw["images"]
            .as_array()
            .is_none_or(|images| images.len() != 2)
    {
        return failure(StatusCode::BAD_GATEWAY, "home_snapshot_incomplete");
    }
    private_json(raw)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_resolves_live_bundle_and_permission_revocation() {
        use zeroclaw_config::schema::{AliasedAgentConfig, McpBundleConfig, RiskProfileConfig};
        let mut config = Config::default();
        config.mcp.enabled = true;
        config.mcp.servers.push(McpServerConfig {
            name: "test-home".into(),
            ..Default::default()
        });
        config.mcp_bundles.insert(
            "home".into(),
            McpBundleConfig {
                servers: vec!["test-home".into()],
                ..Default::default()
            },
        );
        config.agents.insert(
            "observer".into(),
            AliasedAgentConfig {
                enabled: true,
                mcp_bundles: vec!["home".into()],
                risk_profile: "home".into(),
                ..Default::default()
            },
        );
        config.risk_profiles.insert(
            "home".into(),
            RiskProfileConfig {
                deny_all_tools: false,
                auto_approve: vec!["test-home__dashboard".into()],
                ..Default::default()
            },
        );
        assert!(connection(&config, "observer", "test-home", "dashboard").is_ok());
        assert!(connection(&config, "observer", "test-home", "set_power").is_err());
        for gate in ["disabled", "bundle", "excluded", "approval", "deny_all"] {
            let mut revoked = config.clone();
            match gate {
                "disabled" => revoked.agents.get_mut("observer").unwrap().enabled = false,
                "bundle" => revoked
                    .agents
                    .get_mut("observer")
                    .unwrap()
                    .mcp_bundles
                    .clear(),
                "excluded" => revoked
                    .risk_profiles
                    .get_mut("home")
                    .unwrap()
                    .excluded_tools
                    .push("test-home__dashboard".into()),
                "approval" => revoked
                    .risk_profiles
                    .get_mut("home")
                    .unwrap()
                    .always_ask
                    .push("test-home__dashboard".into()),
                _ => {
                    revoked
                        .risk_profiles
                        .get_mut("home")
                        .unwrap()
                        .deny_all_tools = true
                }
            }
            assert!(
                connection(&revoked, "observer", "test-home", "dashboard").is_err(),
                "{gate}"
            );
        }
    }
    #[test]
    fn actions_cannot_select_arbitrary_tools_or_arguments() {
        for value in [
            json!({"action":"call","tool":"shell"}),
            json!({"action":"snapshot","device_id":"other"}),
            json!({"action":"set_power","light":"lamp","state":"toggle"}),
        ] {
            assert!(serde_json::from_value::<HomeAction>(value).is_err());
        }
        assert!(
            serde_json::from_value::<HomeAction>(
                json!({"action":"set_power","light":"lamp","state":"on"})
            )
            .is_ok()
        );
    }
    #[test]
    fn absent_integration_defaults_closed() {
        assert!(owner(&Config::default()).is_err());
    }
}
