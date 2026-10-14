//! Authenticated operator projection of event configuration and delivery receipts.
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
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroclaw_config::events::EventsConfig;
use zeroclaw_runtime::sop::types::SopTrigger;

#[derive(Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EventsResponse {
    pub config: EventsConfig,
    pub revision: String,
    pub rules: Vec<Value>,
    pub records: Vec<Value>,
    pub sops: Vec<String>,
    pub connections: Vec<String>,
    pub runtime_enabled: bool,
}
#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EventsWrite {
    pub config: EventsConfig,
    pub revision: String,
}
#[derive(Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct EventSimulation {
    pub rule: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub duration_secs: u64,
}
fn revision(config: &EventsConfig) -> anyhow::Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(config)?)))
}
fn error(status: StatusCode, message: impl ToString) -> Response {
    (status, Json(json!({"error":message.to_string()}))).into_response()
}

pub async fn get_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read().clone();
    let mut rules = Vec::new();
    let mut records = Vec::new();
    for (id, rule) in &config.events.rules {
        let view = zeroclaw_events::service::rule_directory(&config, id)
            .and_then(|p| zeroclaw_events::store::inspect(&p, 100));
        match view {
            Ok(view) => {
                rules.push(
                    json!({"id":id,"last_poll_ms":view.last_poll_ms,"last_error":view.last_error}),
                );
                for mut row in view.records {
                    row["rule"] = id.clone().into();
                    row["source"] = rule.source.clone().into();
                    row["sop"] = rule.sop.clone().into();
                    records.push(row);
                }
            }
            Err(_) => {
                rules.push(json!({"id":id,"last_poll_ms":null,"last_error":"store_unavailable"}))
            }
        }
    }
    let mut sops = Vec::new();
    if let Some(engine) = &state.sop_engine {
        let engine = match engine.lock() {
            Ok(e) => e,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "SOP engine unavailable"),
        };
        sops = engine.sops().iter().map(|s| s.name.clone()).collect();
        // Derive legacy receipt links from the canonical SOP records; do not
        // persist a second copy of execution state during migration.
        let mut legacy_runs = std::collections::HashMap::new();
        if records.iter().any(|r| r["run_id"].is_null()) {
            for run in engine
                .active_runs()
                .values()
                .chain(engine.finished_runs(None))
            {
                if let Some(payload) = run.trigger_event.payload.as_deref() {
                    if let Ok(value) = serde_json::from_str::<Value>(payload) {
                        if let Some(id) = value["id"].as_str() {
                            legacy_runs.insert(id.to_owned(), run);
                        }
                    }
                }
            }
        }
        for row in &mut records {
            let run = row["run_id"]
                .as_str()
                .and_then(|id| engine.get_run(id))
                .or_else(|| {
                    row["id"]
                        .as_str()
                        .and_then(|id| legacy_runs.get(id).copied())
                });
            if let Some(run) = run {
                row["run_id"] = json!(run.run_id);
                row["sop"] = json!(run.sop_name);
                row["run_status"] = json!(run.status);
                if let Some(step) = run.step_results.first() {
                    if let Ok(decision) = serde_json::from_str::<Value>(&step.output) {
                        row["decision"] = decision;
                    }
                }
                row["notified"] = json!(
                    run.step_results
                        .iter()
                        .flat_map(|s| &s.tool_calls)
                        .any(|c| c.tool == "send_via" && c.success)
                );
            } else if row["run_id"].is_string() {
                row["sop"] = Value::Null;
            }
        }
    }
    records.sort_by_key(|row| std::cmp::Reverse(row["observed_at_ms"].as_i64().unwrap_or(0)));
    records.truncate(200);
    let records = records.into_iter().map(scrub_value).collect();
    let rev = match revision(&config.events) {
        Ok(r) => r,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let connections = config
        .mcp
        .servers
        .iter()
        .filter(|s| s.transport == zeroclaw_config::schema::McpTransport::Http)
        .map(|s| s.name.clone())
        .collect();
    Json(EventsResponse {
        config: config.events,
        revision: rev,
        rules,
        records,
        sops,
        connections,
        runtime_enabled: native_available(&state),
    })
    .into_response()
}

pub async fn put_events(
    State(state): State<AppState>,
    principal: RequestPrincipal,
    Json(body): Json<EventsWrite>,
) -> Response {
    let guard = match state.begin_config_commit().await {
        Ok(g) => g,
        Err(e) => return error(StatusCode::CONFLICT, e),
    };
    let before = guard.current_config();
    if revision(&before.events).ok().as_deref() != Some(&body.revision) {
        return error(
            StatusCode::CONFLICT,
            "Event settings changed; refresh before saving",
        );
    }
    let mut working = before.clone();
    working.events = body.config;
    if let Err(e) = working.validate() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, e);
    }
    for source in working.events.sources.values() {
        if !working.mcp.servers.iter().any(|s| {
            s.name == source.mcp_server
                && s.transport == zeroclaw_config::schema::McpTransport::Http
        }) {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Source needs an existing HTTP MCP connection",
            );
        }
    }
    for id in working.events.rules.keys() {
        let watch = match working.events.watch(id) {
            Ok(w) => w,
            Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e),
        };
        if !watch.enabled {
            continue;
        }
        if !native_available(&state) {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Native events require an event-enabled daemon with the SOP runtime",
            );
        }
        let Some(engine) = &state.sop_engine else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Enable the SOP runtime before enabling rules",
            );
        };
        let engine = match engine.lock() {
            Ok(e) => e,
            Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "SOP engine unavailable"),
        };
        let Some(sop) = engine.get_sop(&watch.sop) else {
            return error(StatusCode::UNPROCESSABLE_ENTITY, "Target SOP is not loaded");
        };
        let Some(owner) = sop.agent.as_deref() else {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Target SOP needs an explicit owner",
            );
        };
        if !working.agents.get(owner).is_some_and(|a| a.enabled)
            || !working.mcp.enabled
            || !working
                .mcp_servers_for_agent(owner)
                .iter()
                .any(|s| s.name == watch.mcp_server)
        {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "SOP owner is disabled or lacks the source connection grant",
            );
        }
        if !sop.triggers.iter().any(|t|matches!(t,SopTrigger::Channel{channel,alias,..} if channel=="event_watch" && alias.as_deref().is_none_or(|a|a==id))){return error(StatusCode::UNPROCESSABLE_ENTITY,"Target SOP needs an event_watch channel trigger matching this rule ID");}
    }
    let mut changed_paths = Vec::new();
    let source_ids = before
        .events
        .sources
        .keys()
        .chain(working.events.sources.keys())
        .collect::<std::collections::BTreeSet<_>>();
    for id in source_ids {
        if before.events.sources.get(id) != working.events.sources.get(id) {
            changed_paths.push(format!("events.sources.{id}"));
        }
    }
    let rule_ids = before
        .events
        .rules
        .keys()
        .chain(working.events.rules.keys())
        .collect::<std::collections::BTreeSet<_>>();
    for id in rule_ids {
        if before.events.rules.get(id) != working.events.rules.get(id) {
            changed_paths.push(format!("events.rules.{id}"));
        }
    }
    let authorization = match authorize_config_write(
        &principal,
        ConfigWriteSet::by_effect(&before, &working, changed_paths.iter().map(String::as_str)),
        &guard,
    ) {
        Ok(a) => a,
        Err(e) => return e.into_response(),
    };
    for path in &changed_paths {
        working.mark_dirty(path);
    }
    // This consumer applies event policy live. Preserve any unrelated pending
    // reload while holding the retained writer through acknowledgement.
    let already_pending = state
        .pending_reload
        .load(std::sync::atomic::Ordering::Relaxed);
    let committed =
        match crate::api_config::persist_and_swap(&state, authorization, working, guard).await {
            Ok(committed) => committed,
            Err(e) => return e,
        };
    if !already_pending {
        state
            .pending_reload
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
    drop(committed);
    Json(json!({"saved":true})).into_response()
}

pub async fn simulate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<EventSimulation>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.read();
    let watch = match config.events.watch(&body.rule) {
        Ok(w) => w,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if let Err(e) = watch.validate() {
        return error(StatusCode::BAD_REQUEST, e);
    }
    let (matched, reason) = if !watch.enabled {
        (false, "disabled")
    } else if watch.source == "fastmail" {
        (true, "mail_candidate")
    } else if !watch.matches_state(&body.state) {
        (false, "condition_not_met")
    } else if body.duration_secs < watch.for_secs {
        (false, "duration_not_met")
    } else {
        (true, "condition_met")
    };
    Json(json!({"matched":matched,"reason":reason,"sop":watch.sop,"side_effects":false}))
        .into_response()
}

fn scrub_value(value: Value) -> Value {
    match value {
        Value::String(s) => Value::String(zeroclaw_runtime::agent::loop_::scrub_credentials(&s)),
        Value::Array(v) => Value::Array(v.into_iter().map(scrub_value).collect()),
        Value::Object(v) => {
            Value::Object(v.into_iter().map(|(k, v)| (k, scrub_value(v))).collect())
        }
        other => other,
    }
}

fn native_available(state: &AppState) -> bool {
    state.sop_engine.is_some()
        && state.sop_driver_handles.is_some()
        && zeroclaw_channels::listing::is_channel_type_compiled("event_watch")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn event_history_and_simulation_require_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, true, false);
        assert_eq!(
            get_events(State(state.clone()), HeaderMap::new())
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            simulate(
                State(state),
                HeaderMap::new(),
                Json(EventSimulation {
                    rule: "fixture".into(),
                    state: "on".into(),
                    duration_secs: 0
                })
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn event_mutation_is_behind_the_config_principal_gate() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, true, false);
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
                    .uri("/api/event-monitor")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"config":{"sources":{},"rules":{}},"revision":"stale"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn stale_event_form_cannot_replace_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::tests::admin_paircode_state(&dir, false, false);
        let response = put_events(
            State(state.clone()),
            None,
            Json(EventsWrite {
                config: EventsConfig::default(),
                revision: "stale".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(state.config.read().events.rules.is_empty());
    }
    #[tokio::test]
    async fn live_event_write_preserves_only_preexisting_reload_requests() {
        use zeroclaw_config::schema::{McpServerConfig, McpTransport};
        for pending in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let state = crate::tests::admin_paircode_state(&dir, false, false);
            let mut config = state.config.read().clone();
            config.mcp.servers.push(McpServerConfig {
                name: "mail".into(),
                transport: McpTransport::Http,
                url: Some("https://example.invalid/mcp".into()),
                ..Default::default()
            });
            state.config_authority.publish_for_test(config.clone());
            state
                .pending_reload
                .store(pending, std::sync::atomic::Ordering::Relaxed);
            let rev = revision(&config.events).unwrap();
            config.events.sources.insert(
                "mail".into(),
                zeroclaw_config::events::EventSource {
                    mcp_server: "mail".into(),
                    ..Default::default()
                },
            );
            let response = put_events(
                State(state.clone()),
                None,
                Json(EventsWrite {
                    config: config.events,
                    revision: rev,
                }),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                state
                    .pending_reload
                    .load(std::sync::atomic::Ordering::Relaxed),
                pending
            );
            assert!(state.config.read().events.sources.contains_key("mail"));
        }
    }
    #[test]
    fn revision_detects_policy_changes_and_scrubs_event_content() {
        let mut config = EventsConfig::default();
        let before = revision(&config).unwrap();
        config.sources.insert("mail".into(), Default::default());
        assert_ne!(before, revision(&config).unwrap());
        let v = scrub_value(json!({"subject":"api_key=sk-secret-fixture-value"}));
        assert!(!v.to_string().contains("sk-secret-fixture-value"));
    }
}
