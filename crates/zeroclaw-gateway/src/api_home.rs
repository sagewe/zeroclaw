//! Private household operator surface. Canonical device state lives in the
//! configured MiJia service; connections and effective grants remain live config.
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
use zeroclaw_runtime::tools::mijia_cli::{CliRequest, run_cli};
use zeroclaw_tools::mcp_client::McpServer;

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
// This local integration is opt-in: a valid pinned package, enabled binding,
// enabled agent, bundle membership and explicit per-tool approval are required.
fn owner(config: &Config) -> Result<(String, String)> {
    let binding = config
        .integration_plugins
        .instances
        .get("mijia")
        .context("home_not_configured")?;
    ensure!(binding.enabled, "home_disabled");
    let manifest = config.integration_manifest("mijia")?;
    if manifest.cli.is_some() {
        let agent = config
            .agents
            .keys()
            .filter(|a| config.integration_cli_allowed(a, "mijia"))
            .min()
            .context("home_permission_required")?;
        return Ok((agent.clone(), "mijia".into()));
    }
    let mut agents: Vec<_> = config.agents.keys().collect();
    agents.sort();
    let agent = agents
        .into_iter()
        .find(|a| connection(config, a, &binding.mcp_server, "dashboard").is_ok())
        .context("home_permission_required")?;
    Ok((agent.clone(), binding.mcp_server.clone()))
}
async fn call(
    state: &AppState,
    headers: &HeaderMap,
    agent: &str,
    server: &str,
    tool: &str,
    args: Value,
) -> Result<Value> {
    let mut cfg = connection(&state.config.read(), agent, server, tool)?;
    let original = serde_json::to_value(&cfg)?;
    let cap = 2 * 1024 * 1024;
    cfg.max_response_bytes = Some(
        cfg.max_response_bytes
            .filter(|n| *n > 0)
            .unwrap_or(cap)
            .min(cap),
    );
    cfg.tool_timeout_secs = Some(cfg.tool_timeout_secs.unwrap_or(50).min(50));
    let client = McpServer::connect(cfg).await?;
    ensure!(require_auth(state, headers).is_ok(), "home_auth_revoked");
    // Re-resolve admission after the asynchronous handshake, before side effects.
    {
        let current = state.config.read();
        let (live_agent, live_server) = owner(&current)?;
        ensure!(
            live_agent == agent && live_server == server,
            "home_scope_changed"
        );
        let live = connection(&current, agent, server, tool)?;
        ensure!(
            serde_json::to_value(&live)? == original,
            "home_scope_changed"
        );
    }
    let raw = client.call_tool(tool, args).await?;
    ensure!(raw["isError"] != true, "home_operation_failed");
    Ok(raw)
}
fn device_allowed(config: &Config, agent: &str, server: &str, tool: &str) -> bool {
    if config
        .integration_manifest("mijia")
        .is_ok_and(|m| m.cli.is_some())
    {
        config.integration_cli_allowed(agent, "mijia")
            && (tool != "set_power"
                || config
                    .risk_profile_for_agent(agent)
                    .is_some_and(|r| r.level != zeroclaw_config::autonomy::AutonomyLevel::ReadOnly))
    } else {
        connection(config, agent, server, tool).is_ok()
    }
}
async fn invoke_device(
    state: &AppState,
    headers: &HeaderMap,
    agent: &str,
    server: &str,
    tool: &str,
    args: Value,
) -> Result<Value> {
    if state
        .config
        .read()
        .integration_manifest("mijia")?
        .cli
        .is_some()
    {
        ensure!(require_auth(state, headers).is_ok(), "home_auth_revoked");
        let request = match tool {
            "camera_snapshot" => json!({"command":"snapshot"}),
            "dashboard" => json!({"command":"dashboard"}),
            "list_devices" => json!({"command":"refresh"}),
            "get_device_spec" => json!({"command":"spec","target":args["device_id"]}),
            "set_power" => json!({"command":"power","target":args["light"],"state":args["state"]}),
            _ => anyhow::bail!("home_unsupported_cli_action"),
        };
        let request: CliRequest = serde_json::from_value(request)?;
        run_cli(&state.config, agent, request).await
    } else {
        data(call(state, headers, agent, server, tool, args).await?)
    }
}
fn data(raw: Value) -> Result<Value> {
    let text = raw["content"]
        .as_array()
        .context("home_response_invalid")?
        .iter()
        .find(|v| v["type"] == "text")
        .and_then(|v| v["text"].as_str())
        .context("home_response_invalid")?;
    Ok(serde_json::from_str(text)?)
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
    let Ok((agent, server)) = owner(&state.config.read()) else {
        return failure(StatusCode::FORBIDDEN, "home_not_available");
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(55),
        invoke_device(&state, &headers, &agent, &server, "dashboard", json!({})),
    )
    .await;
    let Ok(Ok(raw)) = result else {
        return failure(StatusCode::BAD_GATEWAY, "home_service_unavailable");
    };
    let view = raw;
    let config = state.config.read();
    private_json(HomeResponse {
        can_control: device_allowed(&config, &agent, &server, "set_power"),
        can_capture: config.integration_cli_allowed(&agent, "mijia")
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
    let Ok((agent, server)) = owner(&state.config.read()) else {
        return failure(StatusCode::FORBIDDEN, "home_not_available");
    };
    let (server, tool, args) = match &action {
        HomeAction::Refresh {} => (server, "list_devices", json!({})),
        HomeAction::SetPower { light, state } => {
            if light.is_empty() || light.len() > 256 {
                return failure(StatusCode::BAD_REQUEST, "home_invalid_light");
            }
            (
                server,
                "set_power",
                json!({"light":light,"state":match state{PowerState::On=>"on",PowerState::Off=>"off"}}),
            )
        }
        HomeAction::Snapshot {} => (server, "camera_snapshot", json!({})),
        HomeAction::DeviceSpec { device_id } => {
            if device_id.is_empty() || device_id.len() > 128 {
                return failure(StatusCode::BAD_REQUEST, "home_invalid_device");
            }
            (server, "get_device_spec", json!({"device_id":device_id}))
        }
    };
    let permitted = if matches!(action, HomeAction::Snapshot {}) {
        state.config.read().integration_cli_allowed(&agent, "mijia")
    } else {
        device_allowed(&state.config.read(), &agent, &server, tool)
    };
    if !permitted {
        return failure(StatusCode::FORBIDDEN, "home_permission_required");
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(55),
        invoke_device(&state, &headers, &agent, &server, tool, args),
    )
    .await;
    let Ok(Ok(raw)) = result else {
        return failure(
            StatusCode::BAD_GATEWAY,
            "home_operation_unconfirmed_no_retry",
        );
    };
    if matches!(action, HomeAction::Snapshot {}) {
        if raw["images"]
            .as_array()
            .is_none_or(|images| images.len() != 2)
        {
            return failure(StatusCode::BAD_GATEWAY, "home_snapshot_incomplete");
        }
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
