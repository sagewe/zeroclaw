//! Declarative integration packages: references and protocol descriptions, never executable code.
//! Operator config owns activation, connection binding and the admitted manifest digest.
use crate::{
    event_watch::EventWatchConfig,
    events::valid_id,
    schema::{Config, McpTransport},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};
use zeroclaw_macros::Configurable;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
#[prefix = "integration_plugins"]
pub struct IntegrationPluginsConfig {
    #[nested]
    pub instances: HashMap<String, IntegrationBinding>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default)]
pub struct IntegrationBinding {
    pub enabled: bool,
    pub package: String,
    pub manifest_sha256: String,
    pub mcp_server: String,
    /// Select the package's push transport. Polling remains the upgrade default.
    pub push_enabled: bool,
    /// Separate read-only JMAP credential; never included in package files.
    #[secret]
    #[credential_class = "encrypted_secret"]
    pub push_token: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct IntegrationManifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub mcp: RemoteMcp,
    pub event: MailPollSpec,
    pub template: SopTemplate,
    #[serde(default)]
    pub push: Option<JmapPushSpec>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct JmapPushSpec {
    pub session_url: String,
    pub event_origins: Vec<String>,
}
impl JmapPushSpec {
    pub fn validate(&self, mcp_url: &str) -> Result<()> {
        let url = url::Url::parse(&self.session_url)?;
        let mcp = url::Url::parse(mcp_url)?;
        ensure!(
            url.origin() == mcp.origin()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && (url.scheme() == "https"
                    || (url.scheme() == "http"
                        && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))),
            "JMAP session must use the declared MCP origin over HTTPS (or loopback HTTP)"
        );
        ensure!(
            !self.event_origins.is_empty() && self.event_origins.len() <= 8,
            "JMAP push needs an explicit event origin allowlist"
        );
        for origin in &self.event_origins {
            let parsed = url::Url::parse(origin)?;
            ensure!(
                parsed.origin().ascii_serialization() == *origin
                    && (parsed.scheme() == "https"
                        || (parsed.scheme() == "http"
                            && matches!(
                                parsed.host_str(),
                                Some("127.0.0.1" | "localhost" | "[::1]")
                            ))),
                "invalid JMAP event origin"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RemoteMcp {
    pub url: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SopTemplate {
    pub name: String,
    pub description: String,
    pub sop_toml: String,
    pub sop_markdown: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MailPollSpec {
    pub adapter: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub tool: String,
    pub query_argument: String,
    pub limit_argument: String,
    pub cursor_argument: String,
    pub items_pointer: String,
    pub more_pointer: String,
    pub cursor_pointer: String,
    pub id_pointer: String,
    pub time_pointer: String,
    pub subject_pointer: String,
    pub senders_pointer: String,
    pub sender_address_pointer: String,
}
impl IntegrationManifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == 1,
            "unsupported integration package schema"
        );
        ensure!(valid_id(&self.name), "invalid integration package name");
        ensure!(
            !self.version.is_empty()
                && self.version.len() <= 64
                && self.title.len() <= 120
                && self.description.len() <= 2048,
            "invalid integration metadata"
        );
        let url = url::Url::parse(&self.mcp.url)?;
        ensure!(
            matches!(url.scheme(), "https" | "http")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "invalid integration MCP URL"
        );
        self.event.validate()?;
        if let Some(push) = &self.push {
            push.validate(&self.mcp.url)?;
        }
        ensure!(
            valid_id(&self.template.name)
                && self.template.sop_toml.len() <= 4000
                && self.template.sop_markdown.len() <= 16000
                && self.template.description.len() <= 2048,
            "invalid SOP template"
        );
        Ok(())
    }
}
impl MailPollSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.adapter == "mcp_mail_poll_v1" && self.event_type == "email_received",
            "unsupported integration event adapter"
        );
        for name in [
            &self.tool,
            &self.query_argument,
            &self.limit_argument,
            &self.cursor_argument,
        ] {
            ensure!(valid_id(name), "invalid event tool or argument name");
        }
        ensure!(
            self.query_argument != self.limit_argument
                && self.query_argument != self.cursor_argument
                && self.limit_argument != self.cursor_argument,
            "event arguments must be distinct"
        );
        for path in [
            &self.items_pointer,
            &self.more_pointer,
            &self.cursor_pointer,
            &self.id_pointer,
            &self.time_pointer,
            &self.subject_pointer,
            &self.senders_pointer,
            &self.sender_address_pointer,
        ] {
            ensure!(
                path.starts_with('/') && path.len() <= 256 && path.split('/').skip(1).all(valid_id),
                "invalid event JSON pointer"
            );
        }
        Ok(())
    }
}
impl IntegrationPluginsConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.instances.len() <= 64, "too many integration instances");
        let mut servers = HashSet::new();
        for (id, binding) in &self.instances {
            ensure!(
                valid_id(id) && valid_id(&binding.package),
                "invalid integration instance or package id"
            );
            ensure!(
                !binding.mcp_server.is_empty(),
                "integration instance needs a connection"
            );
            ensure!(
                binding.manifest_sha256.len() == 64
                    && binding
                        .manifest_sha256
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit()),
                "integration needs an approved manifest SHA-256"
            );
            if binding.enabled && binding.push_enabled {
                ensure!(
                    binding
                        .push_token
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty()),
                    "JMAP push requires a read-only API token"
                );
            }
            if let Some(token) = &binding.push_token {
                ensure!(
                    token.len() <= 4096 && !token.contains(['\r', '\n']),
                    "invalid JMAP token"
                );
            }
            ensure!(
                servers.insert(&binding.mcp_server),
                "an MCP connection may belong to only one integration instance"
            );
        }
        Ok(())
    }
}
pub fn package_root(config: &Config) -> PathBuf {
    config.shared_workspace_dir().join("integration-plugins")
}
pub fn load_package(root: &Path, name: &str) -> Result<(IntegrationManifest, String)> {
    ensure!(valid_id(name), "invalid integration package id");
    for dir in [root.to_path_buf(), root.join(name)] {
        let meta =
            std::fs::symlink_metadata(&dir).context("integration package directory missing")?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "integration directories must not be symlinks"
        );
    }
    let path = root.join(name).join("integration.toml");
    let meta = std::fs::symlink_metadata(&path).context("integration manifest missing")?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 65536,
        "invalid integration manifest file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&path)?;
    ensure!(
        file.metadata()?.is_file(),
        "integration manifest must be a regular file"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "integration manifest too large");
    let manifest: IntegrationManifest = toml::from_str(std::str::from_utf8(&bytes)?)?;
    manifest.validate()?;
    ensure!(manifest.name == name, "integration directory/name mismatch");
    Ok((manifest, hex::encode(Sha256::digest(&bytes))))
}
/// Compatibility behavior is generated from the same shipped package, not a second Fastmail definition.
pub fn bundled_fastmail() -> Result<IntegrationManifest> {
    let manifest: IntegrationManifest =
        toml::from_str(include_str!("../integrations/fastmail/integration.toml"))?;
    manifest.validate()?;
    Ok(manifest)
}
impl Config {
    pub fn integration_manifest(&self, instance: &str) -> Result<IntegrationManifest> {
        let binding = self
            .integration_plugins
            .instances
            .get(instance)
            .context("integration instance missing")?;
        let (manifest, digest) = load_package(&package_root(self), &binding.package)?;
        ensure!(
            digest.eq_ignore_ascii_case(&binding.manifest_sha256),
            "integration package differs from the operator-approved digest"
        );
        let server = self
            .mcp
            .servers
            .iter()
            .find(|s| s.name == binding.mcp_server)
            .context("integration MCP connection missing")?;
        ensure!(
            server.transport == McpTransport::Http
                && server.url.as_deref() == Some(manifest.mcp.url.as_str()),
            "integration connection does not match its declared MCP endpoint"
        );
        Ok(manifest)
    }
    pub fn integration_allows_mcp(&self, server: &str) -> bool {
        self.integration_plugins
            .instances
            .iter()
            .filter(|(_, b)| b.mcp_server == server)
            .all(|(id, b)| b.enabled && self.integration_manifest(id).is_ok())
    }
    pub fn event_watch(&self, alias: &str) -> Result<EventWatchConfig> {
        let mut view = self.events.watch(alias)?;
        let source = &self.events.sources[&self.events.rules[alias].source];
        if source.kind == "plugin" {
            let binding = self
                .integration_plugins
                .instances
                .get(&source.plugin)
                .context("event integration missing")?;
            self.integration_manifest(&source.plugin)?;
            view.source = "fastmail".into(); // legacy mail checkpoint codec identity
            view.mcp_server = binding.mcp_server.clone();
            view.enabled &= binding.enabled;
        }
        Ok(view)
    }
    pub fn integration_poll_allowed(&self, owner: &str, server: &str, tool: &str) -> bool {
        let Some(risk) = self.risk_profile_for_agent(owner) else {
            return false;
        };
        let name = format!("{server}__{tool}");
        !risk.deny_all_tools
            && !risk.excluded_tools.contains(&name)
            && !risk.always_ask.contains(&name)
            && risk.auto_approve.contains(&name)
    }
    /// Materialize connection inputs from canonical config; callers re-resolve on every policy tick.
    pub fn event_push(&self, alias: &str) -> Result<Option<(JmapPushSpec, String)>> {
        let Some(rule) = self.events.rules.get(alias) else {
            return Ok(None);
        };
        let source = self
            .events
            .sources
            .get(&rule.source)
            .context("event source missing")?;
        if source.kind != "plugin" {
            return Ok(None);
        }
        let binding = self
            .integration_plugins
            .instances
            .get(&source.plugin)
            .context("integration missing")?;
        if !binding.push_enabled {
            return Ok(None);
        }
        ensure!(binding.enabled, "integration disabled");
        let manifest = self.integration_manifest(&source.plugin)?;
        let push = manifest
            .push
            .context("package does not declare JMAP push")?;
        let token = binding
            .push_token
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .context("JMAP push credential missing")?;
        Ok(Some((push, token.clone())))
    }
    pub fn event_mail_spec(&self, alias: &str) -> Result<MailPollSpec> {
        if let Some(rule) = self.events.rules.get(alias) {
            let source = self
                .events
                .sources
                .get(&rule.source)
                .context("event source missing")?;
            if source.kind == "plugin" {
                return Ok(self.integration_manifest(&source.plugin)?.event);
            }
        }
        Ok(bundled_fastmail()?.event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{AliasedAgentConfig, McpBundleConfig, McpServerConfig, RiskProfileConfig};

    #[test]
    fn integration_and_event_fields_use_their_canonical_config_paths() {
        let mut config = Config::default();
        for (path, value) in [
            ("integration_plugins.instances.mail.package", "fastmail"),
            ("integration_plugins.instances.mail.mcp_server", "mail"),
            ("events.sources.mail.plugin", "mail"),
            ("events.sources.mail.kind", "plugin"),
        ] {
            assert!(!config.ensure_map_or_list_key_for_path(path));
            config.set_prop(path, value).unwrap();
            assert_eq!(config.get_prop(path).unwrap(), value);
        }
        assert_eq!(
            config.integration_plugins.instances["mail"].package,
            "fastmail"
        );
        assert_eq!(config.events.sources["mail"].plugin, "mail");
    }

    fn fixture(dir: &Path) -> Config {
        let mut config = Config {
            config_path: dir.join("config.toml"),
            ..Default::default()
        };
        let path = package_root(&config).join("fastmail");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("integration.toml"),
            include_str!("../integrations/fastmail/integration.toml"),
        )
        .unwrap();
        let (_, digest) = load_package(&package_root(&config), "fastmail").unwrap();
        config.mcp.servers.push(McpServerConfig {
            name: "mail".into(),
            transport: McpTransport::Http,
            url: Some(bundled_fastmail().unwrap().mcp.url),
            ..Default::default()
        });
        config.mcp_bundles.insert(
            "mail".into(),
            McpBundleConfig {
                servers: vec!["mail".into()],
                ..Default::default()
            },
        );
        config.agents.insert(
            "observer".into(),
            AliasedAgentConfig {
                mcp_bundles: vec!["mail".into()],
                risk_profile: "observer".into(),
                ..Default::default()
            },
        );
        config
            .risk_profiles
            .insert("observer".into(), RiskProfileConfig::default());
        config.integration_plugins.instances.insert(
            "mail".into(),
            IntegrationBinding {
                enabled: true,
                package: "fastmail".into(),
                manifest_sha256: digest,
                mcp_server: "mail".into(),
                ..Default::default()
            },
        );
        config
    }

    #[tokio::test]
    async fn push_token_is_a_secret_and_is_encrypted_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fixture(dir.path());
        config.data_dir = dir.path().join("data");
        config
            .integration_plugins
            .instances
            .get_mut("mail")
            .unwrap()
            .push_token = Some("jmap-fixture-secret".into());
        assert!(Config::prop_is_secret(
            "integration_plugins.instances.mail.push_token"
        ));
        config.save().await.unwrap();
        let disk = std::fs::read_to_string(&config.config_path).unwrap();
        assert!(!disk.contains("jmap-fixture-secret"));
        let value: toml::Value = toml::from_str(&disk).unwrap();
        assert!(
            value["integration_plugins"]["instances"]["mail"]["push_token"]
                .as_str()
                .unwrap()
                .starts_with("enc2:")
        );
    }

    #[test]
    fn push_activation_requires_credentials_and_approved_origins() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fixture(dir.path());
        config
            .integration_plugins
            .instances
            .get_mut("mail")
            .unwrap()
            .push_enabled = true;
        assert!(config.integration_plugins.validate().is_err());
        config
            .integration_plugins
            .instances
            .get_mut("mail")
            .unwrap()
            .push_token = Some("fixture".into());
        assert!(config.integration_plugins.validate().is_ok());
        let mut push = bundled_fastmail().unwrap().push.unwrap();
        push.session_url = "https://attacker.invalid/session".into();
        assert!(push.validate("https://api.fastmail.com/mcp").is_err());
    }

    #[test]
    fn integration_activation_endpoint_and_pin_gate_existing_grants() {
        assert!(!IntegrationBinding::default().enabled);
        let dir = tempfile::tempdir().unwrap();
        let mut config = fixture(dir.path());
        assert_eq!(config.mcp_servers_for_agent("observer").len(), 1);
        assert!(config.integration_allows_mcp("unbound"));
        config
            .integration_plugins
            .instances
            .get_mut("mail")
            .unwrap()
            .enabled = false;
        assert!(config.mcp_servers_for_agent("observer").is_empty());
        config
            .integration_plugins
            .instances
            .get_mut("mail")
            .unwrap()
            .enabled = true;
        config.mcp.servers[0].url = Some("https://example.invalid/mcp".into());
        assert!(config.mcp_servers_for_agent("observer").is_empty());
        config.mcp.servers[0].url = Some(bundled_fastmail().unwrap().mcp.url);
        let path = package_root(&config).join("fastmail/integration.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{original}\n# changed\n")).unwrap();
        assert!(config.integration_manifest("mail").is_err());
        assert!(config.mcp_servers_for_agent("observer").is_empty());
    }

    #[test]
    fn background_poll_requires_explicit_permission_and_respects_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fixture(dir.path());
        assert!(!config.integration_poll_allowed("observer", "mail", "search_email"));
        config
            .risk_profiles
            .get_mut("observer")
            .unwrap()
            .auto_approve
            .push("mail__search_email".into());
        assert!(config.integration_poll_allowed("observer", "mail", "search_email"));
        for gate in ["always_ask", "excluded", "deny_all"] {
            let mut changed = config.clone();
            let risk = changed.risk_profiles.get_mut("observer").unwrap();
            match gate {
                "always_ask" => risk.always_ask.push("mail__search_email".into()),
                "excluded" => risk.excluded_tools.push("mail__search_email".into()),
                _ => risk.deny_all_tools = true,
            }
            assert!(!changed.integration_poll_allowed("observer", "mail", "search_email"));
        }
    }

    #[test]
    fn integration_packages_reject_traversal_unknown_fields_and_duplicate_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fixture(dir.path());
        assert!(load_package(&package_root(&config), "../fastmail").is_err());
        let binding = config.integration_plugins.instances["mail"].clone();
        config
            .integration_plugins
            .instances
            .insert("duplicate".into(), binding);
        assert!(config.integration_plugins.validate().is_err());
        let original = include_str!("../integrations/fastmail/integration.toml");
        assert!(
            toml::from_str::<IntegrationManifest>(&format!("command='do-not-execute'\n{original}"))
                .is_err()
        );
        let mut manifest = bundled_fastmail().unwrap();
        manifest.mcp.url = "https://user:secret@example.invalid/mcp".into();
        assert!(manifest.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn integration_packages_reject_symlink_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let config = fixture(dir.path());
        let path = package_root(&config).join("fastmail/integration.toml");
        std::fs::rename(&path, dir.path().join("manifest")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("manifest"), path).unwrap();
        assert!(config.integration_manifest("mail").is_err());
    }
}
