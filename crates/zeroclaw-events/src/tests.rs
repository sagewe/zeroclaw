use super::*;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, McpBundleConfig};
use zeroclaw_runtime::sop::types::{Sop, SopExecutionMode, SopPriority, SopStep, SopTrigger};

fn watch() -> EventWatchConfig {
    EventWatchConfig {
        enabled: true,
        source: "home_assistant".into(),
        mcp_server: "fixture".into(),
        sop: "review".into(),
        entities: vec!["light.example".into()],
        states: vec!["unavailable".into()],
        for_secs: 10,
        cooldown_secs: 30,
        ..EventWatchConfig::default()
    }
}
fn home(state: &str) -> Snapshot {
    Snapshot::Home(vec![("light.example".into(), state.into())])
}
fn mail(id: &str, time: i64) -> Value {
    json!({"id":id,"received_at_ms":time,"subject":"Fixture event"})
}

#[test]
fn baseline_duration_recovery_and_cooldown_do_not_flood() {
    let mut cp = Checkpoint::default();
    let w = watch();
    assert!(cp.observe(&w, home("unavailable"), 0).unwrap().is_empty());
    assert!(
        cp.observe(&w, home("unavailable"), 20_000)
            .unwrap()
            .is_empty()
    );
    assert!(cp.observe(&w, home("on"), 21_000).unwrap().is_empty());
    assert!(
        cp.observe(&w, home("unavailable"), 22_000)
            .unwrap()
            .is_empty()
    );
    assert!(
        cp.observe(&w, home("unavailable"), 31_999)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        cp.observe(&w, home("unavailable"), 32_000).unwrap().len(),
        1
    );
    assert!(
        cp.observe(&w, home("unavailable"), 50_000)
            .unwrap()
            .is_empty()
    );
    cp.observe(&w, home("on"), 51_000).unwrap();
    cp.observe(&w, home("unavailable"), 52_000).unwrap();
    assert_eq!(
        cp.observe(&w, home("unavailable"), 62_000).unwrap().len(),
        1
    );
}

#[test]
fn changing_numeric_values_preserve_condition_duration() {
    let w = EventWatchConfig {
        states: vec![],
        above: Some(30.0),
        ..watch()
    };
    let mut cp = Checkpoint::default();
    cp.observe(&w, home("29"), 0).unwrap();
    cp.observe(&w, home("31"), 1000).unwrap();
    cp.observe(&w, home("32"), 5000).unwrap();
    assert_eq!(cp.observe(&w, home("33"), 11000).unwrap().len(), 1);
    assert!(!w.matches_state("NaN"));
    assert!(!w.matches_state("unavailable"));
}

#[test]
fn mail_baseline_and_overlap_survive_restart_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let mut cp = Checkpoint {
        revision: "one".into(),
        ..Checkpoint::default()
    };
    assert!(
        cp.observe(&watch(), Snapshot::Mail(vec![mail("old", 900)]), 1000)
            .unwrap()
            .is_empty()
    );
    store.save(&cp, &[]).unwrap();
    let events = cp
        .observe(
            &watch(),
            Snapshot::Mail(vec![mail("new", 2000), mail("old", 900)]),
            2100,
        )
        .unwrap();
    assert_eq!(events.len(), 1);
    store.save(&cp, &events).unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    let mut recovered = store.load().unwrap();
    assert!(
        recovered
            .observe(&watch(), Snapshot::Mail(vec![mail("new", 2000)]), 3000)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.next().unwrap().unwrap().id, events[0].id);
    // A late arrival inside the overlap is new, despite an older receivedAt.
    assert_eq!(
        recovered
            .observe(&watch(), Snapshot::Mail(vec![mail("late", 1500)]), 3000)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn crash_during_admission_is_not_replayed_and_policy_change_drops_queue() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision: "one".into(),
        ..Checkpoint::default()
    };
    let event = cp.event("condition_entered", 1000, json!({})).unwrap();
    store.save(&cp, std::slice::from_ref(&event)).unwrap();
    store.mark(&event.id, "dispatching").unwrap();
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert!(store.next().unwrap().is_none());
    assert_eq!(store.uncertain_count().unwrap(), 1);
    let other = cp.event("condition_entered", 2000, json!({})).unwrap();
    store.save(&cp, &[other]).unwrap();
    let changed = Checkpoint {
        revision: "two".into(),
        ..Checkpoint::default()
    };
    store.save(&changed, &[]).unwrap();
    assert!(store.next().unwrap().is_none());
}

#[test]
fn failed_store_commit_does_not_advance_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision: "one".into(),
        ..Checkpoint::default()
    };
    store.save(&cp, &[]).unwrap();
    let events = (0..1001)
        .map(|n| cp.event("email_received", n, json!({})).unwrap())
        .collect::<Vec<_>>();
    let next = Checkpoint {
        mail_boundary: Some(10),
        ..cp
    };
    assert!(store.save(&next, &events).is_err());
    assert!(store.load().unwrap().mail_boundary.is_none());
    assert!(store.next().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn store_refuses_symlink_database_and_directory() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    std::fs::create_dir(&target).unwrap();
    let alias = root.path().join("alias");
    symlink(&target, &alias).unwrap();
    assert!(Store::open(&alias).is_err());
    let private = root.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let other = root.path().join("untouched");
    std::fs::write(&other, "fixture").unwrap();
    symlink(&other, private.join("events.db")).unwrap();
    assert!(Store::open(&private).is_err());
    assert_eq!(std::fs::read_to_string(other).unwrap(), "fixture");
}

fn harness(dir: &std::path::Path) -> (EventWatchChannel, zeroclaw_runtime::LiveConfigAuthority) {
    let mut cfg = Config::default();
    cfg.sop.sops_dir = Some("shared/sops".into());
    cfg.channels.event_watch.insert("fixture".into(), watch());
    cfg.mcp.servers.push(McpServerConfig {
        name: "fixture".into(),
        transport: McpTransport::Http,
        url: Some("https://example.invalid/mcp".into()),
        ..Default::default()
    });
    cfg.mcp_bundles.insert(
        "fixture".into(),
        McpBundleConfig {
            servers: vec!["fixture".into()],
            ..Default::default()
        },
    );
    cfg.agents.insert(
        "observer".into(),
        AliasedAgentConfig {
            channels: vec!["event_watch.fixture".into()],
            mcp_bundles: vec!["fixture".into()],
            ..Default::default()
        },
    );
    let mut engine = SopEngine::new(cfg.sop.clone());
    engine.set_sops_for_test(vec![Sop {
        name: "review".into(),
        description: "Fixture".into(),
        version: "1.0.0".into(),
        priority: SopPriority::Normal,
        execution_mode: SopExecutionMode::Supervised,
        triggers: vec![SopTrigger::Channel {
            channel: "event_watch".into(),
            alias: Some("fixture".into()),
            condition: None,
        }],
        steps: vec![SopStep {
            number: 1,
            title: "Review".into(),
            body: "Record an observation".into(),
            ..Default::default()
        }],
        cooldown_secs: 0,
        max_concurrent: 1,
        location: None,
        deterministic: false,
        admission_policy: Default::default(),
        max_pending_approvals: 0,
        agent: Some("observer".into()),
        decision: None,
    }]);
    let engine = Arc::new(Mutex::new(engine));
    let audit = Arc::new(SopAuditLogger::new(Arc::new(
        zeroclaw_memory::none::NoneMemory::new("fixture"),
    )));
    let driver = SopDriverSink::new(
        cfg.clone(),
        engine.clone(),
        Some(audit.clone()),
        Default::default(),
    );
    let authority = zeroclaw_runtime::LiveConfigAuthority::new(cfg);
    (
        EventWatchChannel::new(
            "fixture".into(),
            authority.live_handle(),
            dir.into(),
            engine,
            audit,
            driver,
        ),
        authority,
    )
}

#[tokio::test]
async fn persisted_event_enters_real_sop_ingress_and_is_not_sent_to_chat() {
    let dir = tempfile::tempdir().unwrap();
    let (watch, _authority) = harness(dir.path());
    let (_, _, revision, _) = watch.resolve().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    let event = cp
        .event(
            "condition_entered",
            1000,
            json!({"entity_id":"light.example","state":"unavailable"}),
        )
        .unwrap();
    store.save(&cp, &[event]).unwrap();
    watch.drain(&store).await.unwrap();
    assert!(store.next().unwrap().is_none());
    assert_eq!(watch.engine.lock().unwrap().active_runs().len(), 1);
    assert!(!watch.supports_outbound_send());
}

#[tokio::test]
async fn revoked_live_watch_or_mcp_grant_prevents_queued_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let (channel, authority) = harness(dir.path());
    let (_, _, revision, _) = channel.resolve().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    store
        .save(&cp, &[cp.event("condition_entered", 1, json!({})).unwrap()])
        .unwrap();
    let mut cfg = authority.snapshot_config();
    cfg.channels.event_watch.get_mut("fixture").unwrap().enabled = false;
    authority.publish_for_test(cfg.clone());
    assert!(channel.drain(&store).await.is_err());
    cfg.channels.event_watch.get_mut("fixture").unwrap().enabled = true;
    cfg.agents.get_mut("observer").unwrap().mcp_bundles.clear();
    authority.publish_for_test(cfg);
    assert!(channel.resolve().is_err());
    assert!(channel.engine.lock().unwrap().active_runs().is_empty());
}

#[tokio::test]
async fn ignored_condition_does_not_block_later_matching_event() {
    let dir = tempfile::tempdir().unwrap();
    let (channel, _authority) = harness(dir.path());
    {
        let mut engine = channel.engine.lock().unwrap();
        let mut sop = engine.sops()[0].clone();
        if let SopTrigger::Channel { condition, .. } = &mut sop.triggers[0] {
            *condition = Some("$.event_type == \"email_received\"".into());
        }
        engine.set_sops_for_test(vec![sop]);
    }
    let (_, _, revision, _) = channel.resolve().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    store
        .save(
            &cp,
            &[
                cp.event("condition_entered", 1, json!({})).unwrap(),
                cp.event("email_received", 2, json!({})).unwrap(),
            ],
        )
        .unwrap();
    channel.drain(&store).await.unwrap();
    assert!(store.next().unwrap().is_none());
    assert_eq!(channel.engine.lock().unwrap().active_runs().len(), 1);
}

#[tokio::test]
async fn deterministic_observer_run_completes_through_shared_sop_path() {
    let dir = tempfile::tempdir().unwrap();
    let (channel, _authority) = harness(dir.path());
    {
        let mut engine = channel.engine.lock().unwrap();
        let mut sop = engine.sops()[0].clone();
        sop.execution_mode = SopExecutionMode::Deterministic;
        sop.deterministic = true;
        sop.steps[0].kind = zeroclaw_runtime::sop::types::SopStepKind::Capability;
        sop.steps[0].capability = Some("noop".into());
        engine.set_sops_for_test(vec![sop]);
    }
    let (_, _, revision, _) = channel.resolve().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    store
        .save(&cp, &[cp.event("condition_entered", 1, json!({})).unwrap()])
        .unwrap();
    channel.drain(&store).await.unwrap();
    assert!(store.next().unwrap().is_none());
    let engine = channel.engine.lock().unwrap();
    let runs = engine.finished_runs(Some("review"));
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].status,
        zeroclaw_runtime::sop::types::SopRunStatus::Completed
    );
}

#[test]
fn documented_review_step_has_no_tool_allowance() {
    let doc = include_str!("../../../docs/book/src/channels/event-watch.md");
    let md = doc
        .split("```markdown\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let steps = zeroclaw_runtime::sop::parse_steps(md);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].scope.as_ref().unwrap().allow, Some(vec![]));
}

#[tokio::test]
async fn home_http_source_reads_only_explicit_entities() {
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{header, method, path},
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/states/light.example"))
        .and(header("Authorization", "Bearer fixture"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"entity_id":"light.example","state":"on"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let cfg = McpServerConfig {
        name: "fixture".into(),
        transport: McpTransport::Http,
        url: Some(format!("{}/api/mcp", server.uri())),
        headers: std::collections::HashMap::from([(
            "Authorization".into(),
            "Bearer fixture".into(),
        )]),
        ..Default::default()
    };
    let snapshot = sources::fetch(&watch(), cfg, &Checkpoint::default(), None)
        .await
        .unwrap();
    assert!(
        matches!(snapshot,Snapshot::Home(states) if states==vec![("light.example".into(),"on".into())])
    );
}

#[tokio::test]
async fn fastmail_source_paginates_and_only_collects_metadata() {
    use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request:&Request| {
        let req:Value=serde_json::from_slice(&request.body).unwrap();
        let result=match req["method"].as_str().unwrap() {
            "initialize"=>json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            "notifications/initialized"=>return ResponseTemplate::new(204),
            "tools/list"=>json!({"tools":[{"name":"search_email","description":"Fixture","inputSchema":{"type":"object"}}]}),
            "tools/call"=>{
                assert_eq!(req["params"]["name"],"search_email");
                let second=req["params"]["arguments"]["cursor"].as_str()==Some("next");
                json!({"structuredContent":{"results":[{"id":if second {"two"} else {"one"},"receivedAt":"2026-01-01T00:00:01Z","subject":"Fixture","bodyText":"must not persist"}],"hasMore":!second,"nextCursor":if second {Value::Null} else {json!("next")}}})
            },
            other=>panic!("unexpected MCP operation {other}"),
        };
        ResponseTemplate::new(200).set_body_json(json!({"jsonrpc":"2.0","id":req["id"],"result":result}))
    }).mount(&server).await;
    let cfg = McpServerConfig {
        name: "fixture".into(),
        transport: McpTransport::Http,
        url: Some(server.uri()),
        ..Default::default()
    };
    let watch = EventWatchConfig {
        source: "fastmail".into(),
        ..watch()
    };
    let cp = Checkpoint {
        mail_boundary: Some(0),
        mail_high_water: Some(0),
        ..Default::default()
    };
    let Snapshot::Mail(mails) = sources::fetch(&watch, cfg, &cp, None).await.unwrap() else {
        panic!("wrong snapshot kind")
    };
    assert_eq!(mails.len(), 2);
    assert!(mails.iter().all(|m| m.get("bodyText").is_none()));
    assert_eq!(mails[1]["id"], "two");
}

#[test]
fn native_rule_resolves_without_channel_binding_and_retains_live_grants() {
    let dir = tempfile::tempdir().unwrap();
    let (observer, authority) = harness(dir.path());
    let mut config = authority.snapshot_config();
    config.channels.event_watch.clear();
    config.agents.get_mut("observer").unwrap().channels.clear();
    config.events.sources.insert(
        "ha".into(),
        zeroclaw_config::events::EventSource {
            enabled: true,
            kind: "home_assistant".into(),
            mcp_server: "fixture".into(),
            entities: vec!["light.example".into()],
            ..Default::default()
        },
    );
    config.events.rules.insert(
        "fixture".into(),
        zeroclaw_config::events::EventRule {
            enabled: true,
            source: "ha".into(),
            sop: "review".into(),
            states: vec!["unavailable".into()],
            ..Default::default()
        },
    );
    authority.publish_for_test(config.clone());
    assert!(observer.resolve().is_ok());
    config.events.sources.get_mut("ha").unwrap().enabled = false;
    authority.publish_for_test(config.clone());
    assert!(observer.resolve().is_err());
    config.events.sources.get_mut("ha").unwrap().enabled = true;
    config
        .agents
        .get_mut("observer")
        .unwrap()
        .mcp_bundles
        .clear();
    authority.publish_for_test(config);
    assert!(observer.resolve().is_err());
}
#[test]
fn inspecting_receipts_does_not_recover_an_inflight_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision: "test".into(),
        ..Default::default()
    };
    let event = cp
        .event("email_received", 123, json!({"id":"fixture"}))
        .unwrap();
    store.save(&cp, std::slice::from_ref(&event)).unwrap();
    store.mark(&event.id, "dispatching").unwrap();
    store.poll_succeeded().unwrap();
    let view = store::inspect(dir.path(), 20).unwrap();
    assert_eq!(view.records[0]["disposition"], "dispatching");
    assert!(view.last_poll_ms.is_some());
    assert_eq!(store.uncertain_count().unwrap(), 0);
    store
        .settle(&event.id, "delivered", Some("run-fixture"))
        .unwrap();
    assert_eq!(
        store::inspect(dir.path(), 20).unwrap().records[0]["run_id"],
        "run-fixture"
    );
}
#[test]
fn native_rule_stores_are_isolated_and_ids_cannot_escape() {
    let config = Config::default();
    assert_ne!(
        service::rule_directory(&config, "mail").unwrap(),
        service::rule_directory(&config, "lights").unwrap()
    );
    assert!(service::rule_directory(&config, "../outside").is_err());
}

#[test]
fn pruned_mail_ids_do_not_reenter_from_mixed_age_search_results() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path()).unwrap();
    let mut checkpoint = Checkpoint {
        revision: "fixture".into(),
        ..Default::default()
    };
    checkpoint
        .observe(&watch(), Snapshot::Mail(vec![]), 0)
        .unwrap();
    let first = checkpoint
        .observe(&watch(), Snapshot::Mail(vec![mail("earlier", 1000)]), 1000)
        .unwrap();
    assert_eq!(first.len(), 1);
    let second = checkpoint
        .observe(
            &watch(),
            Snapshot::Mail(vec![mail("latest", 600_000), mail("earlier", 1000)]),
            600_001,
        )
        .unwrap();
    assert_eq!(second.len(), 1);
    assert!(
        !checkpoint.mail_seen.contains_key("earlier"),
        "old IDs leave the bounded overlap"
    );
    store.save(&checkpoint, &second).unwrap();
    let mut restarted = store.load().unwrap();
    for now in [610_000, 670_000, 730_000] {
        let events = restarted
            .observe(
                &watch(),
                Snapshot::Mail(vec![mail("latest", 600_000), mail("earlier", 1000)]),
                now,
            )
            .unwrap();
        assert!(
            events.is_empty(),
            "a pruned historical email was emitted again"
        );
    }
    let late = restarted
        .observe(
            &watch(),
            Snapshot::Mail(vec![mail("late", 400_000)]),
            740_000,
        )
        .unwrap();
    assert_eq!(
        late.len(),
        1,
        "late arrivals within the overlap must still work"
    );
}

#[test]
fn catching_up_a_mail_batch_uses_the_previous_watermark_not_the_newest_item() {
    let mut checkpoint = Checkpoint::default();
    checkpoint
        .observe(&watch(), Snapshot::Mail(vec![]), 0)
        .unwrap();
    let events = checkpoint
        .observe(
            &watch(),
            Snapshot::Mail(vec![mail("newest", 900_000), mail("during_outage", 1000)]),
            900_001,
        )
        .unwrap();
    assert_eq!(
        events.len(),
        2,
        "catch-up cannot discard unobserved mail within the prior cursor range"
    );
    assert!(
        checkpoint
            .observe(
                &watch(),
                Snapshot::Mail(vec![mail("newest", 900_000), mail("during_outage", 1000)]),
                960_000
            )
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn integration_manifest_drives_wire_tool_arguments_and_projection() {
    use wiremock::{Mock, MockServer, Request, ResponseTemplate, matchers::method};
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(|request: &Request| {
        let req: Value = serde_json::from_slice(&request.body).unwrap();
        let result = match req["method"].as_str().unwrap() {
            "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            "notifications/initialized" => return ResponseTemplate::new(204),
            "tools/list" => json!({"tools":[{"name":"find_messages","inputSchema":{"type":"object"}}]}),
            "tools/call" => {
                assert_eq!(req["params"]["name"], "find_messages");
                assert_eq!(req["params"]["arguments"]["filter"], "in:inbox");
                assert_eq!(req["params"]["arguments"]["count"], 50);
                json!({"structuredContent":{"messages":[{"key":"one","at":"2026-01-01T00:00:01Z","subject":"Fixture","from":[{"email":"sender@example.invalid"}],"body":"private"}],"hasMore":false}})
            },
            other => panic!("unexpected MCP operation {other}"),
        };
        ResponseTemplate::new(200).set_body_json(json!({"jsonrpc":"2.0","id":req["id"],"result":result}))
    }).mount(&server).await;
    let mut spec = bundled_fastmail().unwrap().event.into_mail().unwrap();
    spec.tool = "find_messages".into();
    spec.query_argument = "filter".into();
    spec.limit_argument = "count".into();
    spec.items_pointer = "/messages".into();
    spec.id_pointer = "/key".into();
    spec.time_pointer = "/at".into();
    let cfg = McpServerConfig {
        name: "fixture".into(),
        transport: McpTransport::Http,
        url: Some(server.uri()),
        ..Default::default()
    };
    let w = EventWatchConfig {
        source: "fastmail".into(),
        ..watch()
    };
    let Snapshot::Mail(mails) = sources::fetch(&w, cfg, &Checkpoint::default(), Some(&spec))
        .await
        .unwrap()
    else {
        panic!("wrong adapter");
    };
    assert_eq!(mails[0]["id"], "one");
    assert_eq!(mails[0]["from"], json!(["sender@example.invalid"]));
    assert!(mails[0].get("body").is_none());
}

#[tokio::test]
async fn integration_migration_preserves_checkpoint_and_pause_blocks_queued_dispatch() {
    use zeroclaw_config::{
        events::{EventRule, EventSource},
        integration_plugins::{IntegrationBinding, load_package, package_root},
    };
    let dir = tempfile::tempdir().unwrap();
    let (observer, authority) = harness(dir.path());
    let mut config = authority.snapshot_config();
    config.config_path = dir.path().join("config.toml");
    config.channels.event_watch.clear();
    config.events.sources.insert(
        "mail".into(),
        EventSource {
            enabled: true,
            kind: "fastmail".into(),
            mcp_server: "fixture".into(),
            ..Default::default()
        },
    );
    config.events.rules.insert(
        "fixture".into(),
        EventRule {
            enabled: true,
            source: "mail".into(),
            sop: "review".into(),
            ..Default::default()
        },
    );
    config.agents.get_mut("observer").unwrap().risk_profile = "observer".into();
    config.risk_profiles.insert(
        "observer".into(),
        zeroclaw_config::schema::RiskProfileConfig {
            auto_approve: vec!["fixture__search_email".into()],
            ..Default::default()
        },
    );
    authority.publish_for_test(config.clone());
    let (_, _, legacy_revision, _) = observer.resolve().unwrap();
    let path = package_root(&config).join("fastmail");
    std::fs::create_dir_all(&path).unwrap();
    let mut manifest = bundled_fastmail().unwrap();
    manifest.mcp = Some(zeroclaw_config::integration_plugins::RemoteMcp::Absolute {
        url: "https://example.invalid/mcp".into(),
    });
    // Serialize through JSON -> TOML is unnecessary: use the canonical package bytes and replace only fixture endpoint.
    let bytes = include_str!("../../zeroclaw-config/integrations/fastmail/integration.toml")
        .replace("https://api.fastmail.com", "https://example.invalid");
    std::fs::write(path.join("integration.toml"), bytes).unwrap();
    let (_, digest) = load_package(&package_root(&config), "fastmail").unwrap();
    config.integration_plugins.instances.insert(
        "mail".into(),
        IntegrationBinding {
            enabled: true,
            package: "fastmail".into(),
            manifest_sha256: digest,
            mcp_server: "fixture".into(),
            ..Default::default()
        },
    );
    let source = config.events.sources.get_mut("mail").unwrap();
    source.kind = "plugin".into();
    source.plugin = "mail".into();
    source.mcp_server.clear();
    authority.publish_for_test(config.clone());
    let (_, _, revision, _) = observer.resolve().unwrap();
    assert_eq!(
        legacy_revision, revision,
        "equivalent migration must not reset mail deduplication"
    );
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    let event = cp
        .event("email_received", 1, json!({"id":"queued"}))
        .unwrap();
    store.save(&cp, &[event]).unwrap();
    config
        .integration_plugins
        .instances
        .get_mut("mail")
        .unwrap()
        .enabled = false;
    authority.publish_for_test(config.clone());
    assert!(observer.drain(&store).await.is_err());
    assert!(store.next().unwrap().is_some());
    config
        .integration_plugins
        .instances
        .get_mut("mail")
        .unwrap()
        .enabled = true;
    config
        .risk_profiles
        .get_mut("observer")
        .unwrap()
        .auto_approve
        .clear();
    authority.publish_for_test(config);
    assert!(observer.resolve().is_err());
}

#[test]
fn bundled_integration_template_loads_with_narrow_step_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("review-mail");
    std::fs::create_dir(&path).unwrap();
    let template = bundled_fastmail().unwrap().template;
    let expand = |s: String| {
        s.replace("{{agent}}", "observer")
            .replace("{{rule}}", "mail")
            .replace("{{mcp_server}}", "mail")
            .replace("{{notification_peer}}", "owner")
    };
    std::fs::write(path.join("SOP.toml"), expand(template.sop_toml)).unwrap();
    std::fs::write(path.join("SOP.md"), expand(template.sop_markdown)).unwrap();
    let sops =
        zeroclaw_runtime::sop::load_sops_from_directory(dir.path(), SopExecutionMode::Supervised);
    assert_eq!(sops.len(), 1);
    assert_eq!(sops[0].steps.len(), 2);
    assert_eq!(
        sops[0].steps[0].scope.as_ref().unwrap().allow,
        Some(vec!["mail__read_email".into()])
    );
    assert_eq!(
        sops[0].steps[1].scope.as_ref().unwrap().allow,
        Some(vec!["send_via".into()])
    );
}

#[tokio::test]
async fn home_plugin_preserves_legacy_identity_and_requires_live_read_permission() {
    use zeroclaw_config::{
        events::{EventRule, EventSource},
        integration_plugins::{IntegrationBinding, load_package, package_root},
    };
    let dir = tempfile::tempdir().unwrap();
    let (observer, authority) = harness(dir.path());
    let mut config = authority.snapshot_config();
    config.config_path = dir.path().join("config.toml");
    config.mcp.servers[0].url = Some("https://example.invalid/api/mcp".into());
    config.mcp.servers[0]
        .headers
        .insert("Authorization".into(), "Bearer fixture".into());
    config.agents.get_mut("observer").unwrap().risk_profile = "observer".into();
    config.risk_profiles.insert(
        "observer".into(),
        zeroclaw_config::schema::RiskProfileConfig {
            auto_approve: vec!["fixture__homeassistant__GetLiveContext".into()],
            ..Default::default()
        },
    );
    authority.publish_for_test(config.clone());
    let (_, _, legacy, _) = observer.resolve().unwrap();
    let path = package_root(&config).join("homeassistant");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("integration.toml"),
        include_str!("../../zeroclaw-config/integrations/homeassistant/integration.toml"),
    )
    .unwrap();
    let (_, digest) = load_package(&package_root(&config), "homeassistant").unwrap();
    config.integration_plugins.instances.insert(
        "home".into(),
        IntegrationBinding {
            enabled: true,
            package: "homeassistant".into(),
            manifest_sha256: digest,
            mcp_server: "fixture".into(),
            push_enabled: true,
            ..Default::default()
        },
    );
    config.channels.event_watch.clear();
    config.events.sources.insert(
        "home".into(),
        EventSource {
            enabled: true,
            kind: "plugin".into(),
            plugin: "home".into(),
            entities: vec!["light.example".into()],
            ..Default::default()
        },
    );
    config.events.rules.insert(
        "fixture".into(),
        EventRule {
            enabled: true,
            source: "home".into(),
            sop: "review".into(),
            states: vec!["unavailable".into()],
            for_secs: 10,
            cooldown_secs: 30,
            ..Default::default()
        },
    );
    authority.publish_for_test(config.clone());
    let (_, _, revision, _) = observer.resolve().unwrap();
    assert_eq!(legacy, revision);
    let store = Store::open(dir.path()).unwrap();
    let cp = Checkpoint {
        revision,
        ..Default::default()
    };
    store
        .save(
            &cp,
            &[cp.event(
                "condition_entered",
                1000,
                json!({"entity_id":"light.example","state":"unavailable"}),
            )
            .unwrap()],
        )
        .unwrap();
    config
        .risk_profiles
        .get_mut("observer")
        .unwrap()
        .auto_approve
        .clear();
    authority.publish_for_test(config);
    assert!(observer.drain(&store).await.is_err());
    assert!(store.next().unwrap().is_some());
}
