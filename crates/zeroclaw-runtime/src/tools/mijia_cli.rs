//! Typed, bounded argv execution for the explicitly granted MiJia CLI integration.
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::{
    live::LiveConfigHandle,
    policy::{SecurityPolicy, ToolOperation},
    schema::RuntimeKind,
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum CliRequest {
    Dashboard {},
    Cameras {},
    Snapshot {
        #[serde(default)]
        resolution: Option<String>,
    },
    Status {},
    Refresh {},
    Devices {
        #[serde(default)]
        room: Option<String>,
        #[serde(default)]
        search: Option<String>,
        #[serde(default)]
        online: bool,
    },
    Switches {
        #[serde(default)]
        room: Option<String>,
        #[serde(default)]
        search: Option<String>,
        #[serde(default)]
        online: bool,
    },
    Show {
        target: String,
        #[serde(default)]
        room: Option<String>,
    },
    Spec {
        target: String,
        #[serde(default)]
        room: Option<String>,
    },
    Read {
        target: String,
        properties: Vec<String>,
        #[serde(default)]
        room: Option<String>,
    },
    Power {
        target: String,
        state: PowerState,
    },
    Events {
        #[serde(default)]
        device: Option<String>,
        #[serde(default)]
        room: Option<String>,
    },
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    On,
    Off,
}
impl CliRequest {
    pub fn writes(&self) -> bool {
        matches!(self, Self::Power { .. })
    }
    pub fn argv(&self) -> Result<Vec<String>> {
        let valid = |s: &str| -> Result<()> {
            ensure!(
                !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control),
                "mijia_cli_invalid_argument"
            );
            Ok(())
        };
        let mut args = Vec::new();
        let mut option = |name: &str, v: &Option<String>| -> Result<()> {
            if let Some(v) = v {
                valid(v)?;
                args.push(format!("--{name}={v}"));
            }
            Ok(())
        };
        let (command, positional) = match self {
            Self::Cameras {} => ("cameras", vec![]),
            Self::Snapshot { resolution } => {
                option("resolution", resolution)?;
                ("snapshot", vec![])
            }
            Self::Dashboard {} => ("dashboard", vec![]),
            Self::Status {} => ("status", vec![]),
            Self::Refresh {} => ("refresh", vec![]),
            Self::Devices {
                room,
                search,
                online,
            }
            | Self::Switches {
                room,
                search,
                online,
            } => {
                option("room", room)?;
                option("search", search)?;
                if *online {
                    args.push("--online".into());
                }
                (
                    if matches!(self, Self::Devices { .. }) {
                        "devices"
                    } else {
                        "switches"
                    },
                    vec![],
                )
            }
            Self::Show { target, room }
            | Self::Spec { target, room }
            | Self::Read { target, room, .. } => {
                valid(target)?;
                option("room", room)?;
                let mut positions = vec![target.clone()];
                let command = match self {
                    Self::Show { .. } => "show",
                    Self::Spec { .. } => "spec",
                    Self::Read { properties, .. } => {
                        ensure!(
                            (1..=16).contains(&properties.len()),
                            "mijia_cli_invalid_properties"
                        );
                        for p in properties {
                            let (s, i) = p.split_once('.').context("mijia_cli_invalid_property")?;
                            ensure!(
                                [s, i].iter().all(|v| v
                                    .parse::<u32>()
                                    .is_ok_and(|n| (1..=65535).contains(&n))),
                                "mijia_cli_invalid_property"
                            );
                            positions.push(p.clone());
                        }
                        "read"
                    }
                    _ => anyhow::bail!("mijia_cli_invalid_command"),
                };
                (command, positions)
            }
            Self::Power { target, state } => {
                valid(target)?;
                (
                    "power",
                    vec![
                        target.clone(),
                        match state {
                            PowerState::On => "on".into(),
                            PowerState::Off => "off".into(),
                        },
                    ],
                )
            }
            Self::Events { device, room } => {
                option("device", device)?;
                option("room", room)?;
                ("events", vec![])
            }
        };
        args.insert(0, command.into());
        if !positional.is_empty() {
            args.push("--".into());
            args.extend(positional);
        }
        Ok(args)
    }
}
async fn bounded(reader: impl tokio::io::AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= limit, "mijia_cli_output_limit");
    Ok(bytes)
}
/// Resolve all policy at dispatch. Both the agent tool and operator gateway use
/// this same subprocess boundary; there is no shell interpolation or retry.
pub async fn run_cli(live: &LiveConfigHandle, agent: &str, request: CliRequest) -> Result<Value> {
    let argv = request.argv()?;
    let snapshot = matches!(request, CliRequest::Snapshot { .. });
    let output_limit = if snapshot {
        9 * 1024 * 1024
    } else {
        2 * 1024 * 1024
    };
    let (mut child, sandbox) = {
        let config = live.read();
        ensure!(
            config.integration_cli_allowed(agent, "mijia"),
            "mijia_cli_not_authorized"
        );
        ensure!(
            config.runtime.kind == RuntimeKind::Native,
            "mijia_cli_requires_native_runtime"
        );
        let risk = config
            .risk_profile_for_agent(agent)
            .context("mijia_cli_missing_profile")?;
        let security = SecurityPolicy::for_agent(&config, agent)?;
        security
            .authorize_tool_operation(
                if request.writes() {
                    ToolOperation::Act
                } else {
                    ToolOperation::Read
                },
                "mijia_cli",
            )
            .map_err(anyhow::Error::msg)?;
        let binding = &config.integration_plugins.instances["mijia"];
        let program = Path::new(&binding.cli_command);
        ensure!(
            program.is_absolute() && program.is_file(),
            "mijia_cli_executable_missing"
        );
        // The explicit CLI binding grants this invocation read access to its
        // executable and two connection files, not generic shell/file tools.
        let mut cli_read_only = security.allowed_roots_read_only.clone();
        cli_read_only.extend([
            program.to_path_buf(),
            Path::new(&binding.cli_root).join("policy.json"),
            Path::new(&binding.cli_root).join("access.token"),
        ]);
        let sandbox = crate::security::create_sandbox(
            &risk.sandbox_config(),
            RuntimeKind::Native,
            Some(&security.workspace_dir),
            &crate::security::SandboxExtraRoots {
                read_write: security.allowed_roots.clone(),
                read_only: cli_read_only,
                write_only: security.allowed_roots_write_only.clone(),
            },
        );
        sandbox.check_initialization()?;
        ensure!(
            sandbox.coding_cli_unsupported_reason().is_none(),
            "mijia_cli_sandbox_unsupported"
        );
        let mut command = tokio::process::Command::new(program);
        command
            .args(["--root", &binding.cli_root, "--json"])
            .args(argv)
            .current_dir(&security.workspace_dir);
        sandbox.wrap_command(command.as_std_mut())?;
        // Replacing sandbox wrappers discard stdio and environment settings.
        // Apply these to the final executable, after wrapping.
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        (command.spawn().context("mijia_cli_spawn_failed")?, sandbox)
    };
    let stdout = child.stdout.take().context("mijia_cli_stdout_missing")?;
    let stderr = child.stderr.take().context("mijia_cli_stderr_missing")?;
    let (stdout, stderr, status) = tokio::time::timeout(Duration::from_secs(55), async {
        tokio::try_join!(
            bounded(stdout, output_limit),
            bounded(stderr, 16384),
            async { Ok::<_, anyhow::Error>(child.wait().await?) }
        )
    })
    .await
    .context("mijia_cli_timeout_no_retry")??;
    // Seatbelt owns a policy file read by the child after spawn; keep its
    // per-invocation owner alive until the process exits, including cancellation.
    drop(sandbox);
    if !status.success() {
        let reason = serde_json::from_slice::<Value>(&stderr)
            .ok()
            .and_then(|v| {
                v["error"].as_str().map(|s| {
                    s.chars()
                        .filter(|c| !c.is_control())
                        .take(512)
                        .collect::<String>()
                })
            })
            .unwrap_or_else(|| "mijia_cli_process_failed".into());
        anyhow::bail!("mijia_cli_failed_or_unconfirmed_no_retry: {reason}");
    }
    if snapshot {
        ensure!(
            live.read().integration_cli_allowed(agent, "mijia"),
            "mijia_cli_not_authorized"
        );
    }
    serde_json::from_slice(&stdout).context("mijia_cli_invalid_json")
}
/// Keep image bytes out of model text. Only the explicit snapshot command can
/// declare media, using the same workspace materializer as other tool payloads.
fn snapshot_output(mut value: Value, workspace: &Path) -> Result<ToolOutput> {
    use base64::Engine;
    use zeroclaw_api::media::{MarkerKind, RenderedMarker};
    let images = value
        .as_object_mut()
        .context("mijia_snapshot_invalid")?
        .remove("images")
        .context("mijia_snapshot_invalid")?;
    let images = images.as_array().context("mijia_snapshot_invalid")?;
    ensure!(images.len() == 2, "mijia_snapshot_incomplete");
    // Validate every part before materializing any image.
    for (channel, img) in images.iter().enumerate() {
        ensure!(
            img["channel"] == channel && img["mime_type"] == "image/jpeg",
            "mijia_snapshot_invalid"
        );
        let encoded = img["data"].as_str().context("mijia_snapshot_invalid")?;
        ensure!(encoded.len() <= 4 * 1024 * 1024, "mijia_snapshot_too_large");
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
        ensure!(
            bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.ends_with(&[0xff, 0xd9]),
            "mijia_snapshot_invalid"
        );
    }
    value["frames"]=json!(images.iter().map(|image|json!({"channel":image["channel"],"width":image["width"],"height":image["height"]})).collect::<Vec<_>>());
    let mut output = ToolOutput::text(value.to_string());
    for (channel, img) in images.iter().enumerate() {
        let image = zeroclaw_tools::embedded_resource::materialize_resource_blob(
            workspace,
            Some(&format!("mijia-channel-{channel}.jpg")),
            Some("image/jpeg"),
            img["data"].as_str().context("mijia_snapshot_invalid")?,
        )?;
        output = output.with_attachment(RenderedMarker {
            target: image.abs_path.to_string_lossy().into_owned(),
            kind: MarkerKind::Image,
        });
    }
    Ok(output)
}
pub struct MijiaCliTool {
    live: Option<LiveConfigHandle>,
    agent: String,
    description: String,
}
impl MijiaCliTool {
    pub fn new(live: Option<LiveConfigHandle>, agent: String) -> Self {
        Self {
            live,
            agent,
            description: crate::i18n::get_required_cli_string("cli-mijia-tool-description"),
        }
    }
}
#[async_trait]
impl Tool for MijiaCliTool {
    fn name(&self) -> &str {
        "mijia_cli"
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{"command":{"type":"string","enum":["dashboard","status","devices","switches","show","spec","read","power","events","refresh","cameras","snapshot"]},"resolution":{"type":"string","description":crate::i18n::get_required_cli_string("cli-mijia-resolution-description")},"target":{"type":"string"},"state":{"type":"string","enum":["on","off"]},"room":{"type":"string"},"search":{"type":"string"},"online":{"type":"boolean"},"device":{"type":"string"},"properties":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":16}},"required":["command"],"additionalProperties":false})
    }
    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let snapshot = args["command"] == "snapshot";
        let result = match (&self.live, serde_json::from_value(args)) {
            (Some(live), Ok(request)) => run_cli(live, &self.agent, request).await,
            (None, _) => Err(anyhow::Error::msg("mijia_cli_live_config_required")),
            (_, Err(_)) => Err(anyhow::Error::msg("mijia_cli_invalid_request")),
        };
        let result = result.and_then(|v| {
            if snapshot {
                let live = self
                    .live
                    .as_ref()
                    .context("mijia_cli_live_config_required")?;
                let config = live.read();
                ensure!(
                    config.integration_cli_allowed(&self.agent, "mijia"),
                    "mijia_cli_not_authorized"
                );
                snapshot_output(
                    v,
                    &SecurityPolicy::for_agent(&config, &self.agent)?.workspace_dir,
                )
            } else {
                Ok(ToolOutput::text(v.to_string()))
            }
        });
        Ok(match result {
            Ok(v) => ToolResult {
                success: true,
                output: v,
                error: None,
            },
            Err(e) => ToolResult {
                success: false,
                output: ToolOutput::default(),
                error: Some(crate::i18n::get_required_cli_string_with_args(
                    "cli-mijia-tool-failed",
                    &[("reason", &e.to_string())],
                )),
            },
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_declares_two_images_and_rejects_arbitrary_capture_arguments() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let bytes = [0xff, 0xd8, 0xff, 0xd9];
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let value = json!({"captured_at_ms":1,"images":[{"channel":0,"width":848,"height":480,"mime_type":"image/jpeg","data":encoded},{"channel":1,"width":848,"height":480,"mime_type":"image/jpeg","data":encoded}]});
        let output = snapshot_output(value.clone(), dir.path()).unwrap();
        assert_eq!(output.attachments().len(), 2);
        assert!(!output.as_str().contains(&encoded));
        let metadata: Value = serde_json::from_str(output.as_str()).unwrap();
        assert_eq!(
            metadata["frames"][0],
            json!({"channel":0,"width":848,"height":480})
        );
        for image in output.attachments() {
            assert_eq!(std::fs::read(&image.target).unwrap(), bytes);
            assert!(image.target.ends_with(".jpg"));
        }
        let mut invalid = value;
        invalid["images"][1]["data"] = json!("not jpeg");
        let empty = tempfile::tempdir().unwrap();
        assert!(snapshot_output(invalid, empty.path()).is_err());
        assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(), 0);
        for extra in ["device_id", "output", "audio", "worker", "root"] {
            let mut v = json!({"command":"snapshot"});
            v[extra] = json!("arbitrary");
            assert!(serde_json::from_value::<CliRequest>(v).is_err());
        }
        assert_eq!(
            CliRequest::Snapshot { resolution: None }.argv().unwrap(),
            vec!["snapshot"]
        );
    }
    #[test]
    fn snapshot_resolution_is_a_literal_option_and_scoped_to_capture() {
        for resolution in [
            "low",
            "high",
            "848x480",
            "2560x1440",
            "--root=/tmp; touch /tmp/no",
        ] {
            let request: CliRequest =
                serde_json::from_value(json!({"command":"snapshot","resolution":resolution}))
                    .unwrap();
            assert_eq!(
                request.argv().unwrap(),
                vec!["snapshot".to_string(), format!("--resolution={resolution}")]
            );
        }
        for value in [
            json!({"command":"snapshot","resolution":3}),
            json!({"command":"devices","resolution":"high"}),
            json!({"command":"snapshot","quality":"high"}),
        ] {
            assert!(serde_json::from_value::<CliRequest>(value).is_err());
        }
    }
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn subprocess_keeps_seatbelt_policy_alive_until_exit() {
        use std::os::unix::fs::PermissionsExt;
        use zeroclaw_config::{
            integration_plugins::{IntegrationBinding, load_package, package_root},
            live::LiveConfig,
            schema::{AliasedAgentConfig, Config, RiskProfileConfig},
        };
        let dir = tempfile::Builder::new()
            .prefix("mijia-cli-sandbox-test-")
            .tempdir_in(std::env::var_os("HOME").unwrap())
            .unwrap();
        let connection_root = dir.path().join("connection");
        std::fs::create_dir_all(&connection_root).unwrap();
        for name in ["policy.json", "access.token", "ungranted.txt"] {
            let path = connection_root.join(name);
            std::fs::write(&path, "fixture").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.mcp.enabled = false;
        let package = package_root(&config).join("mijia");
        std::fs::create_dir_all(&package).unwrap();
        let manifest =
            include_str!("../../../zeroclaw-config/integrations/homeassistant/integration.toml")
                .replace(
                    r#"[mcp]
path = "/api/mcp""#,
                    r#"[cli]
protocol = "mijia_cli_v1""#,
                )
                .replace("name = \"homeassistant\"", "name = \"mijia\"");
        std::fs::write(package.join("integration.toml"), manifest).unwrap();
        let (_, digest) = load_package(&package_root(&config), "mijia").unwrap();
        let executable = dir.path().join("fixture-cli");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
test -z "${HOME+x}" || exit 9
cat "$2/policy.json" "$2/access.token" >/dev/null || exit 10
if cat "$2/ungranted.txt" >/dev/null 2>&1; then exit 11; fi
if (printf changed > "$2/policy.json") 2>/dev/null; then exit 12; fi
sleep 0.02
printf '{"ready":true}\n'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        config.integration_plugins.instances.insert(
            "mijia".into(),
            IntegrationBinding {
                enabled: true,
                package: "mijia".into(),
                manifest_sha256: digest,
                cli_command: executable.display().to_string(),
                cli_root: connection_root.display().to_string(),
                ..Default::default()
            },
        );
        config.agents.insert(
            "observer".into(),
            AliasedAgentConfig {
                enabled: true,
                risk_profile: "observer".into(),
                ..Default::default()
            },
        );
        config.risk_profiles.insert(
            "observer".into(),
            RiskProfileConfig {
                auto_approve: vec!["mijia_cli".into()],
                workspace_only: true,
                sandbox_enabled: Some(true),
                sandbox_backend: Some("seatbelt".into()),
                ..Default::default()
            },
        );
        let security = SecurityPolicy::for_agent(&config, "observer").unwrap();
        std::fs::create_dir_all(&security.workspace_dir).unwrap();
        let live = LiveConfig::new(config.clone());
        for _ in 0..3 {
            assert_eq!(
                run_cli(&live.handle(), "observer", CliRequest::Status {})
                    .await
                    .unwrap(),
                json!({"ready":true})
            );
        }
        // Optional operator-invoked acceptance against the installed local CLI.
        // It only requests status and never writes to physical devices.
        if let Some(root) = std::env::var_os("MIJIA_LIVE_TEST_ROOT") {
            let root = std::path::PathBuf::from(root);
            let binding = config
                .integration_plugins
                .instances
                .get_mut("mijia")
                .unwrap();
            binding.cli_root = root.display().to_string();
            binding.cli_command = root.join("bin/mijia").display().to_string();
            let live = LiveConfig::new(config);
            let result = run_cli(&live.handle(), "observer", CliRequest::Status {})
                .await
                .unwrap();
            assert_eq!(result["ready"], true);
            if std::env::var_os("MIJIA_LIVE_TEST_CAPTURE").is_some() {
                let tool = MijiaCliTool::new(Some(live.handle()), "observer".into());
                let result = tool
                    .execute(json!({"command":"snapshot","resolution":"2560x1440"}))
                    .await
                    .unwrap();
                assert!(result.success, "{:?}", result.error);
                assert_eq!(result.output.attachments().len(), 2);
                let metadata: Value = serde_json::from_str(result.output.as_str()).unwrap();
                assert_eq!(metadata["resolution"], "2560x1440");
                assert_eq!(metadata["frames"][0]["width"], 2560);
            }
        }
    }
    #[test]
    fn argv_never_interprets_target_as_shell_or_option() {
        let request = CliRequest::Power {
            target: "$(touch /tmp/never); --root=/tmp".into(),
            state: PowerState::Off,
        };
        assert_eq!(
            request.argv().unwrap(),
            vec!["power", "--", "$(touch /tmp/never); --root=/tmp", "off"]
        );
    }
    #[test]
    fn raw_commands_root_overrides_and_watch_are_rejected() {
        for v in [
            json!({"command":"shell","target":"id"}),
            json!({"command":"dashboard","root":"/tmp"}),
            json!({"command":"events","watch":true}),
            json!({"command":"power","target":"lamp","state":"toggle"}),
        ] {
            assert!(serde_json::from_value::<CliRequest>(v).is_err());
        }
        assert!(
            CliRequest::Read {
                target: "device".into(),
                properties: vec!["0.1".into()],
                room: None
            }
            .argv()
            .is_err()
        );
    }
}
