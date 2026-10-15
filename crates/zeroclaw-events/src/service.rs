//! Native event lifecycle. Config owns membership; SQLite owns observations.
use crate::{EventWatchChannel, store::Store};
use anyhow::Context;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::channel::Channel;
use zeroclaw_config::{live::LiveConfigHandle, schema::Config};
use zeroclaw_runtime::sop::{SopAuditLogger, SopDriverSink, SopEngine};

pub fn rule_directory(config: &Config, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        zeroclaw_config::events::valid_id(id),
        "invalid event rule id"
    );
    Ok(config.data_dir.join("events").join(id))
}

pub struct EventService {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}
impl EventService {
    pub fn start(
        live: LiveConfigHandle,
        engine: Arc<Mutex<SopEngine>>,
        audit: Arc<SopAuditLogger>,
        driver: SopDriverSink,
    ) -> Self {
        let cancel = CancellationToken::new();
        let stopped = cancel.clone();
        let handle = zeroclaw_spawn::spawn!(async move {
            let mut active: HashMap<
                String,
                (String, CancellationToken, tokio::task::JoinHandle<()>),
            > = HashMap::new();
            loop {
                let desired = {
                    let config = live.read();
                    config
                        .events
                        .rules
                        .keys()
                        .filter_map(|id| {
                            let watch = config.event_watch(id).ok()?;
                            if !watch.enabled {
                                return None;
                            }
                            let signature = serde_json::to_string(&watch).ok()?;
                            let path = rule_directory(&config, id).ok()?;
                            Some((id.clone(), signature, path))
                        })
                        .collect::<Vec<_>>()
                };
                let retired = active
                    .iter()
                    .filter(|(id, (sig, _, handle))| {
                        handle.is_finished()
                            || !desired.iter().any(|(a, s, _)| a == *id && s == sig)
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in retired {
                    if let Some((_, token, handle)) = active.remove(&id) {
                        token.cancel();
                        if let Err(error) = handle.await {
                            zeroclaw_log::record!(
                                ERROR,
                                zeroclaw_log::Event::new(
                                    module_path!(),
                                    zeroclaw_log::Action::Fail
                                )
                                .with_attrs(serde_json::json!({"error":error.to_string()})),
                                "Event worker stopped unexpectedly"
                            );
                        }
                    }
                }
                for (id, signature, path) in desired {
                    if active.contains_key(&id) {
                        continue;
                    }
                    // Serialize policy replacement by draining existing tasks below on changes.
                    let token = stopped.child_token();
                    let worker_token = token.clone();
                    let observer = EventWatchChannel::new(
                        id.clone(),
                        live.clone(),
                        path.clone(),
                        engine.clone(),
                        audit.clone(),
                        driver.clone(),
                    );
                    let handle = zeroclaw_spawn::spawn!(async move {
                        let token = worker_token;
                        let (tx, _rx) = tokio::sync::mpsc::channel(1);
                        loop {
                            tokio::select! {
                                biased;
                                _=token.cancelled()=>break,
                                result=observer.listen(tx.clone())=> {
                                    if let Err(error)=result {
                                        let recorded=Store::open(&path).and_then(|store|store.poll_failed());
                                        let detail=zeroclaw_runtime::agent::loop_::scrub_credentials(&error.to_string());
                                        zeroclaw_log::record!(WARN,zeroclaw_log::Event::new(module_path!(),zeroclaw_log::Action::Fail).with_attrs(serde_json::json!({"error":detail,"health_recorded":recorded.is_ok()})),"Event observation failed; retrying with bounded backoff");
                                    }
                                }
                            }
                            tokio::select! {_=token.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(15))=>{}}
                        }
                    });
                    active.insert(id, (signature, token, handle));
                }
                tokio::select! {_=stopped.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
            }
            for (_, (_, token, handle)) in active {
                token.cancel();
                if let Err(error) = handle.await {
                    zeroclaw_log::record!(
                        WARN,
                        zeroclaw_log::Event::new(module_path!(), zeroclaw_log::Action::Fail)
                            .with_attrs(serde_json::json!({"error":error.to_string()})),
                        "Event worker shutdown failed"
                    );
                }
            }
        });
        Self { cancel, handle }
    }
    pub async fn stop(self) -> anyhow::Result<()> {
        self.cancel.cancel();
        self.handle
            .await
            .context("event supervisor shutdown failed")?;
        Ok(())
    }
}
