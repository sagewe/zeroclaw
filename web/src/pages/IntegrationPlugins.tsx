import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  Package,
  RefreshCw,
  Plus,
  ArrowRight,
  FileText,
  X,
} from "lucide-react";
import { Badge, Button, Card, PageHeader } from "@/components/ui";
import { tLocale, type Locale } from "@/lib/i18n";
import { useLocaleContext } from "@/App";
import {
  getIntegrationPlugins,
  saveIntegrationPlugins,
  type IntegrationCatalog,
  type IntegrationBinding,
  type IntegrationConfig,
} from "@/lib/integrationPlugins";
const homePackage = (
  manifest: IntegrationCatalog["packages"][number]["manifest"],
) => manifest?.event.adapter === "home_assistant_state_v1";
const input =
  "w-full rounded-lg border border-pc-border bg-pc-base px-3 py-2 text-sm text-pc-text";
const empty: IntegrationBinding = {
  enabled: false,
  package: "",
  manifest_sha256: "",
  mcp_server: "",
  cli_command: "",
  cli_root: "",
  push_enabled: false,
  push_token: null,
};
export default function IntegrationPlugins() {
  const { locale } = useLocaleContext();
  const t = (key: string) => tLocale(key, locale as Locale);
  const [data, setData] = useState<IntegrationCatalog | null>(null),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false);
  const [edit, setEdit] = useState<{
    id: string;
    existing: boolean;
    binding: IntegrationBinding;
    revision: string;
    approvedDigest: string | null;
  } | null>(null);
  const [template, setTemplate] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const refresh = useCallback(async () => {
    try {
      setData(await getIntegrationPlugins());
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, [refresh]);
  const save = async (config: IntegrationConfig, revision: string) => {
    setBusy(true);
    setError("");
    try {
      await saveIntegrationPlugins(config, revision);
      setEdit(null);
      setSaved(true);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const pkg = edit
    ? data?.packages.find((p) => p.package === edit.binding.package)
    : undefined;
  const chosenTemplate = data?.packages.find((p) => p.package === template)
    ?.manifest?.template;
  return (
    <div className="p-6 md:p-8 space-y-6 max-w-[1400px] mx-auto">
      <PageHeader
        title={t("integrations_plugin.title")}
        description={t("integrations_plugin.description")}
        actions={
          <Button variant="ghost" onClick={() => void refresh()}>
            <RefreshCw size={14} />
            {t("events.refresh")}
          </Button>
        }
      />
      <nav className="flex gap-4 text-sm border-b border-pc-border pb-3">
        <span className="text-pc-accent">{t("integrations_plugin.title")}</span>
        <Link to="/plugins" className="text-pc-text-secondary">
          {t("integrations_plugin.wasm")}
        </Link>
      </nav>
      {error && (
        <div
          role="alert"
          className="p-3 rounded-lg bg-status-error/10 text-status-error"
        >
          {error}
        </div>
      )}
      {saved && (
        <p role="status" className="text-sm text-pc-accent">
          {t("integrations_plugin.saved")}
        </p>
      )}
      {!data ? (
        <p>{t("events.loading")}</p>
      ) : (
        <>
          {data.discovery_error && (
            <p role="alert" className="text-status-warning">
              {t("integrations_plugin.discovery_error")}
            </p>
          )}
          <div
            className={
              "grid gap-6 " + (edit ? "lg:grid-cols-[minmax(0,1fr)_380px]" : "")
            }
          >
            <section className="space-y-4">
              <div className="flex justify-between items-center">
                <h2 className="font-semibold">
                  {t("integrations_plugin.instances")}
                </h2>
                <Button
                  onClick={() =>
                    setEdit({
                      id: "",
                      existing: false,
                      binding: { ...empty },
                      revision: data.revision,
                      approvedDigest: null,
                    })
                  }
                >
                  <Plus size={14} />
                  {t("integrations_plugin.bind")}
                </Button>
              </div>
              {Object.entries(data.config.instances).map(([id, binding]) => {
                const p = data.packages.find(
                  (p) => p.package === binding.package,
                );
                const invalid = data.invalid_instances.includes(id);
                return (
                  <Card key={id} className="p-5 space-y-4">
                    <div className="flex justify-between items-start gap-4">
                      <div className="flex gap-3">
                        <Package className="text-pc-accent mt-1" size={24} />
                        <div>
                          <h2 className="font-semibold text-pc-text">
                            {p?.manifest?.title ?? binding.package}
                          </h2>
                          <p className="text-xs text-pc-text-secondary mt-1">
                            {id} · {p?.manifest?.version ?? "—"}
                          </p>
                        </div>
                      </div>
                      <Badge
                        tone={
                          invalid ? "error" : binding.enabled ? "ok" : "neutral"
                        }
                      >
                        {t(
                          invalid
                            ? "integrations_plugin.invalid"
                            : binding.enabled
                              ? "events.enabled"
                              : "events.paused",
                        )}
                      </Badge>
                    </div>
                    <p className="text-sm text-pc-text-secondary">
                      {t(
                        binding.cli_command
                          ? "integrations_plugin.cli_capabilities"
                          : "integrations_plugin.capabilities",
                      )}
                    </p>
                    <div className="grid sm:grid-cols-2 gap-3">
                      <Link
                        to={
                          binding.cli_command
                            ? "/config/integration_plugins.instances/" +
                              encodeURIComponent(id)
                            : "/config/mcp.servers/" +
                              encodeURIComponent(binding.mcp_server)
                        }
                        className="p-3 bg-pc-elevated rounded-lg text-sm"
                      >
                        {binding.cli_command
                          ? "CLI · " + binding.cli_command.split("/").pop()
                          : "MCP · " + binding.mcp_server}
                        <p className="mt-1 text-xs text-pc-text-secondary">
                          {t("integrations_plugin.agent_policy")}
                        </p>
                      </Link>
                      <Link
                        to="/events/sources"
                        className="p-3 bg-pc-elevated rounded-lg text-sm"
                      >
                        {p?.manifest?.event.type ?? "email_received"}
                        <p className="mt-1 text-xs text-pc-text-secondary">
                          {t(
                            binding.push_enabled
                              ? homePackage(p?.manifest ?? null)
                                ? "integrations_plugin.home_push_mode"
                                : "integrations_plugin.push_mode"
                              : "integrations_plugin.polling",
                          )}
                        </p>
                      </Link>
                    </div>
                    {invalid && (
                      <p className="text-sm text-status-warning">
                        {t("integrations_plugin.invalid_hint")}
                      </p>
                    )}
                    <div className="flex flex-wrap gap-2 border-t border-pc-border pt-3">
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={busy || (!binding.enabled && invalid)}
                        onClick={() =>
                          void save(
                            {
                              ...data.config,
                              instances: {
                                ...data.config.instances,
                                [id]: { ...binding, enabled: !binding.enabled },
                              },
                            },
                            data.revision,
                          )
                        }
                      >
                        {t(
                          binding.enabled
                            ? "integrations_plugin.disable"
                            : "integrations_plugin.enable",
                        )}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() =>
                          setEdit({
                            id,
                            existing: true,
                            binding: { ...binding },
                            revision: data.revision,
                            approvedDigest: null,
                          })
                        }
                      >
                        {t("events.edit")}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setTemplate(binding.package)}
                      >
                        <FileText size={13} />
                        {t("integrations_plugin.template")}
                      </Button>
                      <Link
                        to="/events/rules"
                        className="ml-auto inline-flex items-center gap-1 text-xs text-pc-accent"
                      >
                        {t("events.rules")}
                        <ArrowRight size={13} />
                      </Link>
                    </div>
                  </Card>
                );
              })}
              {!Object.keys(data.config.instances).length && (
                <Card className="p-8 text-center text-pc-text-secondary">
                  {t("integrations_plugin.no_instances")}
                </Card>
              )}
              <h2 className="font-semibold pt-3">
                {t("integrations_plugin.installed")}
              </h2>
              {data.packages.map((p) => (
                <Card key={p.package} className="p-4 space-y-2">
                  <div className="flex justify-between">
                    <span className="font-medium">
                      {p.manifest?.title ?? p.package}
                    </span>
                    <Badge>
                      {p.manifest?.version ?? t("integrations_plugin.invalid")}
                    </Badge>
                  </div>
                  <p className="text-xs text-pc-text-secondary">
                    {p.manifest?.description ??
                      t("integrations_plugin.invalid_hint")}
                  </p>
                  {p.manifest && (
                    <p className="text-xs text-pc-text-secondary">
                      {p.manifest.cli
                        ? "CLI: " + p.manifest.cli.protocol
                        : p.manifest.mcp
                          ? "MCP: " +
                            ("url" in p.manifest.mcp
                              ? p.manifest.mcp.url
                              : p.manifest.mcp.path)
                          : "—"}
                      <br />
                      {t(
                        homePackage(p.manifest)
                          ? "integrations_plugin.read_permission"
                          : "integrations_plugin.poll_tool",
                      )}
                      :{" "}
                      {"tool" in p.manifest.event
                        ? p.manifest.event.tool
                        : p.manifest.event.permission_tool}
                    </p>
                  )}
                  <code className="block break-all text-[11px] text-pc-text-muted">
                    SHA-256: {p.sha256 ?? "—"}
                  </code>
                </Card>
              ))}
              <p className="text-xs text-pc-text-muted">
                {t("integrations_plugin.install_hint")}
              </p>
            </section>
            {edit && (
              <Card className="p-5 space-y-4 self-start">
                <div className="flex justify-between">
                  <h2 className="font-semibold">
                    {t("integrations_plugin.binding")}
                  </h2>
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={t("events.close")}
                    onClick={() => setEdit(null)}
                  >
                    <X size={14} />
                  </Button>
                </div>
                <form
                  className="space-y-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (
                      !pkg?.sha256 ||
                      (edit.approvedDigest ?? edit.binding.manifest_sha256) !==
                        pkg.sha256 ||
                      (!edit.existing && edit.id in data.config.instances)
                    )
                      return;
                    void save(
                      {
                        instances: {
                          ...data.config.instances,
                          [edit.id]: {
                            ...edit.binding,
                            manifest_sha256:
                              edit.approvedDigest ??
                              edit.binding.manifest_sha256,
                          },
                        },
                      },
                      edit.revision,
                    );
                  }}
                >
                  <label className="block text-sm space-y-1">
                    <span>{t("events.id")}</span>
                    <input
                      className={input}
                      value={edit.id}
                      disabled={edit.existing}
                      required
                      pattern="[A-Za-z0-9_-]{1,64}"
                      onChange={(e) => setEdit({ ...edit, id: e.target.value })}
                    />
                  </label>
                  <label className="block text-sm space-y-1">
                    <span>{t("integrations_plugin.package")}</span>
                    <select
                      className={input}
                      required
                      value={edit.binding.package}
                      onChange={(e) =>
                        setEdit({
                          ...edit,
                          binding: {
                            ...edit.binding,
                            package: e.target.value,
                            mcp_server: "",
                            cli_command: "",
                            cli_root: "",
                            manifest_sha256: "",
                          },
                          approvedDigest: null,
                        })
                      }
                    >
                      <option value="">{t("events.select")}</option>
                      {data.packages
                        .filter((p) => p.manifest)
                        .map((p) => (
                          <option key={p.package} value={p.package}>
                            {p.manifest?.title}
                          </option>
                        ))}
                    </select>
                  </label>
                  {pkg?.manifest?.cli ? (
                    <div className="space-y-3">
                      {(["cli_command", "cli_root"] as const).map((key) => (
                        <label key={key} className="block text-sm space-y-1">
                          <span>{t("integrations_plugin." + key)}</span>
                          <input
                            className={input}
                            required
                            value={edit.binding[key]}
                            onChange={(e) =>
                              setEdit({
                                ...edit,
                                binding: {
                                  ...edit.binding,
                                  mcp_server: "",
                                  [key]: e.target.value,
                                },
                              })
                            }
                          />
                        </label>
                      ))}
                      <p className="text-xs text-pc-text-secondary">
                        {t("integrations_plugin.cli_hint")}
                      </p>
                    </div>
                  ) : (
                    <>
                      <label className="block text-sm space-y-1">
                        <span>{t("events.connection")}</span>
                        <select
                          className={input}
                          required
                          value={edit.binding.mcp_server}
                          onChange={(e) =>
                            setEdit({
                              ...edit,
                              binding: {
                                ...edit.binding,
                                mcp_server: e.target.value,
                              },
                            })
                          }
                        >
                          <option value="">{t("events.select")}</option>
                          {data.connections
                            .filter((c) =>
                              c.packages.includes(edit.binding.package),
                            )
                            .map((c) => (
                              <option key={c.name}>{c.name}</option>
                            ))}
                        </select>
                      </label>
                      <p className="text-xs text-pc-text-secondary">
                        {t("integrations_plugin.connection_hint")}
                      </p>
                    </>
                  )}
                  {pkg?.manifest?.push && (
                    <div className="space-y-3 border-t border-pc-border pt-3">
                      <label className="flex items-center gap-2 text-sm">
                        <input
                          type="checkbox"
                          checked={edit.binding.push_enabled}
                          onChange={(e) =>
                            setEdit({
                              ...edit,
                              binding: {
                                ...edit.binding,
                                push_enabled: e.target.checked,
                              },
                            })
                          }
                        />
                        {t(
                          homePackage(pkg.manifest)
                            ? "integrations_plugin.home_push_mode"
                            : "integrations_plugin.push_mode",
                        )}
                      </label>
                      {!homePackage(pkg.manifest) && (
                        <label className="block text-sm space-y-1">
                          <span>{t("integrations_plugin.push_token")}</span>
                          <input
                            type="password"
                            autoComplete="new-password"
                            className={input}
                            value={edit.binding.push_token ?? ""}
                            onChange={(e) =>
                              setEdit({
                                ...edit,
                                binding: {
                                  ...edit.binding,
                                  push_token: e.target.value || null,
                                },
                              })
                            }
                            placeholder={t(
                              data.push_credentials_configured.includes(edit.id)
                                ? "integrations_plugin.token_saved"
                                : "integrations_plugin.token_missing",
                            )}
                          />
                        </label>
                      )}
                      <p className="text-xs text-pc-text-secondary">
                        {t(
                          homePackage(pkg.manifest)
                            ? "integrations_plugin.home_push_hint"
                            : "integrations_plugin.push_hint",
                        )}
                      </p>
                    </div>
                  )}
                  {pkg?.sha256 !== edit.binding.manifest_sha256 && (
                    <label className="flex items-start gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={edit.approvedDigest === pkg?.sha256}
                        onChange={(e) =>
                          setEdit({
                            ...edit,
                            approvedDigest: e.target.checked
                              ? (pkg?.sha256 ?? null)
                              : null,
                          })
                        }
                      />
                      <span>
                        {t("integrations_plugin.approve")}
                        <code className="block text-[10px] break-all mt-2">
                          {pkg?.sha256}
                        </code>
                      </span>
                    </label>
                  )}
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={edit.binding.enabled}
                      onChange={(e) =>
                        setEdit({
                          ...edit,
                          binding: {
                            ...edit.binding,
                            enabled: e.target.checked,
                          },
                        })
                      }
                    />
                    {t("events.enabled")}
                  </label>
                  <p className="text-xs text-pc-text-muted">
                    {t("integrations_plugin.reload_hint")}
                  </p>
                  <Button
                    type="submit"
                    disabled={
                      busy ||
                      !edit.id ||
                      !pkg?.sha256 ||
                      (!edit.existing && edit.id in data.config.instances) ||
                      (edit.approvedDigest ?? edit.binding.manifest_sha256) !==
                        pkg.sha256
                    }
                  >
                    {t("events.save")}
                  </Button>
                </form>
              </Card>
            )}
          </div>
          {chosenTemplate && (
            <Card className="p-5 space-y-3">
              <div className="flex justify-between">
                <h2 className="font-semibold">
                  {t("integrations_plugin.template")} · {chosenTemplate.name}
                </h2>
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setTemplate(null)}
                  aria-label={t("events.close")}
                >
                  <X size={14} />
                </Button>
              </div>
              <p className="text-sm text-pc-text-secondary">
                {t("integrations_plugin.template_hint")}
              </p>
              <pre className="p-3 bg-pc-elevated rounded-lg whitespace-pre-wrap break-words text-xs max-h-96 overflow-auto">
                {chosenTemplate.sop_toml + "\n" + chosenTemplate.sop_markdown}
              </pre>
            </Card>
          )}
        </>
      )}
    </div>
  );
}
