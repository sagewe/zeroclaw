import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import {
  Activity,
  ArrowRight,
  CheckCircle2,
  Database,
  Mail,
  Plus,
  Radio,
  RefreshCw,
  Settings2,
  Workflow,
  X,
} from "lucide-react";
import { Badge, Button, Card, PageHeader } from "@/components/ui";
import { tLocale, type Locale } from "@/lib/i18n";
import { useLocaleContext } from "@/App";
import {
  getEvents,
  saveEvents,
  simulateEvent,
  type EventsState,
  type EventsConfig,
  type EventSource,
  type EventRule,
} from "@/lib/events";
import { sourceStatus, parseEntities } from "./events.logic";
const input =
  "w-full rounded-lg border border-pc-border bg-pc-base px-3 py-2 text-sm text-pc-text focus:outline-none focus:ring-2 focus:ring-pc-accent";
const emptySource: EventSource = {
  enabled: false,
  kind: "fastmail",
  mcp_server: "",
  plugin: "",
  poll_interval_secs: 60,
  query: "in:inbox",
  entities: [],
};
const emptyRule: EventRule = {
  enabled: false,
  source: "",
  sop: "",
  states: [],
  above: null,
  below: null,
  for_secs: 60,
  cooldown_secs: 300,
};
const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="block space-y-1.5 text-sm text-pc-text-secondary">
      <span>{label}</span>
      {children}
    </label>
  );
}
export default function Events() {
  const { locale } = useLocaleContext();
  const t = (key: string) => tLocale(key, locale as Locale);
  const { tab = "sources" } = useParams();
  const [params] = useSearchParams();
  const [data, setData] = useState<EventsState | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [editor, setEditor] = useState<{
    kind: "source" | "rule";
    id: string;
    existing: boolean;
    value: EventSource | EventRule;
    revision: string;
  } | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected = data?.records.find((r) => r.id === selectedId) ?? null;
  const [filter, setFilter] = useState("");
  const [test, setTest] = useState<{
    rule: string;
    state: string;
    duration: number;
    result?: string;
  } | null>(null);
  const load = useCallback(async () => {
    try {
      setData(await getEvents());
      setError("");
    } catch (e) {
      setError(message(e));
    }
  }, []);
  useEffect(() => {
    void load();
    const timer = setInterval(() => void load(), 10000);
    return () => clearInterval(timer);
  }, [load]);
  useEffect(() => {
    setEditor(null);
    setTest(null);
    setSelectedId(null);
  }, [tab]);
  const mutate = async (config: EventsConfig, revision = data?.revision) => {
    if (!revision) return;
    setBusy(true);
    try {
      await saveEvents(config, revision);
      setEditor(null);
      await load();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const date = (ms?: number | null) =>
    ms ? new Date(ms).toLocaleString() : t("events.never");
  const edit = (
    kind: "source" | "rule",
    id = "",
    value?: EventSource | EventRule,
  ) => {
    if (!data) return;
    setError("");
    setEditor({
      kind,
      id,
      existing: !!id,
      value: structuredClone(
        value ?? (kind === "source" ? emptySource : emptyRule),
      ),
      revision: data.revision,
    });
  };
  const field = (key: string, value: unknown) =>
    setEditor((e) => (e ? { ...e, value: { ...e.value, [key]: value } } : e));
  const sourceRules = (id: string) =>
    Object.entries(data?.config.rules ?? {}).filter(([, r]) => r.source === id);
  const sourceKind = (source: EventSource) =>
    source.kind === "plugin"
      ? (data?.integration_kinds[source.plugin] ?? "fastmail")
      : source.kind;
  const effectiveSource = (id: string) =>
    data?.resolved_sources[id] ?? data?.config.sources[id];
  const health = (id: string) => data?.rules.find((r) => r.id === id);
  const lastPoll = Math.max(
    0,
    ...(data?.rules.map((h) => h.last_poll_ms ?? 0) ?? []),
  );
  const records =
    data?.records.filter(
      (r) =>
        (!params.get("run") || r.run_id === params.get("run")) &&
        (!filter ||
          JSON.stringify([r.rule, r.source, r.data, r.decision])
            .toLowerCase()
            .includes(filter.toLowerCase())),
    ) ?? [];
  return (
    <div className="p-5 md:p-8 space-y-6 max-w-[1500px] mx-auto animate-fade-in">
      <PageHeader
        title={t("events.title")}
        description={t("events.description")}
        actions={
          <Button variant="ghost" onClick={() => void load()}>
            <RefreshCw size={14} />
            {t("events.refresh")}
          </Button>
        }
      />
      {error && (
        <div
          role="alert"
          className="rounded-lg border border-status-error/30 bg-status-error/10 p-3 text-sm text-status-error"
        >
          {error}
        </div>
      )}
      {!data ? (
        <Card className="p-8 text-pc-text-secondary">
          {t("events.loading")}
        </Card>
      ) : (
        <>
          {!data.runtime_enabled && (
            <Card className="p-3 text-status-warning">
              {t("events.runtime_off")}
            </Card>
          )}
          <div className="grid gap-3 sm:grid-cols-3">
            {[
              [
                t("events.sources"),
                String(Object.keys(data.config.sources).length),
                Radio,
              ],
              [
                t("events.active_rules"),
                String(
                  Object.values(data.config.rules).filter(
                    (r) => r.enabled && effectiveSource(r.source)?.enabled,
                  ).length,
                ),
                Workflow,
              ],
              [t("events.last_poll"), date(lastPoll), Activity],
            ].map(([label, value, Icon]) => {
              const I = Icon as typeof Radio;
              return (
                <Card
                  key={String(label)}
                  className="p-4 flex items-start gap-3"
                >
                  <I size={18} className="text-pc-text-muted mt-1" />
                  <div>
                    <p className="text-xs text-pc-text-secondary">
                      {String(label)}
                    </p>
                    <p className="mt-1 font-medium text-pc-text">
                      {String(value)}
                    </p>
                  </div>
                </Card>
              );
            })}
          </div>
          <div
            className="flex gap-1 border-b border-pc-border overflow-x-auto"
            role="navigation"
            aria-label={t("events.title")}
          >
            {["sources", "rules", "history"].map((v) => (
              <Link
                key={v}
                to={"/events/" + v}
                className={
                  "px-4 py-3 text-sm border-b-2 whitespace-nowrap " +
                  (v === tab
                    ? "border-pc-accent text-pc-text"
                    : "border-transparent text-pc-text-secondary hover:text-pc-text")
                }
              >
                {t("events." + v)}
              </Link>
            ))}
          </div>
          <div
            className={
              "grid gap-6 " +
              (editor || selected ? "lg:grid-cols-[minmax(0,1fr)_400px]" : "")
            }
          >
            <section className="min-w-0 space-y-4">
              {tab === "sources" && (
                <>
                  <div className="flex justify-between gap-4 items-center">
                    <p className="text-sm text-pc-text-secondary">
                      {t("events.sources_hint")}
                    </p>
                    <Button onClick={() => edit("source")}>
                      <Plus size={15} />
                      {t("events.add_source")}
                    </Button>
                  </div>
                  {Object.entries(data.config.sources).map(([id, source]) => {
                    const rules = sourceRules(id);
                    const display = data.resolved_sources[id] ?? {
                      ...source,
                      valid: true,
                    };
                    const status =
                      display.valid === false
                        ? "error"
                        : sourceStatus(
                            display.enabled,
                            data.resolved_sources[id]?.push
                              ? 30
                              : source.poll_interval_secs,
                            rules.map(([rid, r]) => ({
                              enabled: r.enabled,
                              ...health(rid),
                            })),
                          );
                    const recent = Math.max(
                      0,
                      ...rules.map(([rid]) => health(rid)?.last_poll_ms ?? 0),
                    );
                    return (
                      <Card key={id} className="p-5 space-y-4">
                        <div className="flex justify-between gap-3">
                          <div className="flex gap-3 items-center">
                            <div className="p-2.5 rounded-xl bg-pc-elevated text-pc-accent">
                              {display.kind === "fastmail" ? (
                                <Mail size={22} />
                              ) : (
                                <Radio size={22} />
                              )}
                            </div>
                            <div>
                              <h2 className="font-semibold text-pc-text">
                                {id}
                              </h2>
                              <p className="text-xs text-pc-text-secondary mt-1">
                                {display.kind === "fastmail"
                                  ? "Fastmail"
                                  : "Home Assistant"}{" "}
                                ·{" "}
                                {data.resolved_sources[id]?.push
                                  ? t(
                                      display.kind === "home_assistant"
                                        ? "integrations_plugin.home_push_mode"
                                        : "integrations_plugin.push_mode",
                                    )
                                  : `${t("events.polling")} ${source.poll_interval_secs}s`}
                              </p>
                            </div>
                          </div>
                          <Badge
                            tone={
                              status === "healthy"
                                ? "ok"
                                : status === "error"
                                  ? "error"
                                  : status === "stale"
                                    ? "warn"
                                    : "neutral"
                            }
                          >
                            {t("events.status." + status)}
                          </Badge>
                        </div>
                        <div className="grid sm:grid-cols-3 gap-3 text-sm">
                          <div>
                            <p className="text-xs text-pc-text-muted mb-1">
                              {t("events.scope")}
                            </p>
                            <p className="text-pc-text break-words">
                              {display.kind === "fastmail"
                                ? source.query
                                : source.entities.join(", ")}
                            </p>
                          </div>
                          <div>
                            <p className="text-xs text-pc-text-muted mb-1">
                              {t(
                                data.resolved_sources[id]?.push
                                  ? "events.push_activity"
                                  : "events.last_poll",
                              )}
                            </p>
                            <p className="text-pc-text">{date(recent)}</p>
                          </div>
                          <div>
                            <p className="text-xs text-pc-text-muted mb-1">
                              {t("events.rules")}
                            </p>
                            <Link className="text-pc-accent" to="/events/rules">
                              {rules.length} {t("events.rule_count")}
                            </Link>
                          </div>
                        </div>
                        {(status === "error" || status === "stale") && (
                          <p className="text-sm text-status-warning">
                            {t("events.connection_error")}
                          </p>
                        )}
                        {source.kind === "plugin" && (
                          <Link
                            className="block text-xs text-pc-accent"
                            to="/plugins/integrations"
                          >
                            {t("integrations_plugin.package")} · {source.plugin}
                          </Link>
                        )}
                        <div className="flex gap-2 pt-2 border-t border-pc-border">
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => edit("source", id, source)}
                          >
                            <Settings2 size={13} />
                            {t("events.edit")}
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={busy}
                            onClick={() =>
                              void mutate({
                                ...data.config,
                                sources: {
                                  ...data.config.sources,
                                  [id]: { ...source, enabled: !source.enabled },
                                },
                              })
                            }
                          >
                            {t(
                              source.enabled ? "events.pause" : "events.enable",
                            )}
                          </Button>
                          <Link
                            to={
                              "/config/mcp.servers/" +
                              encodeURIComponent(display.mcp_server)
                            }
                            className="ml-auto text-xs self-center text-pc-text-secondary"
                          >
                            {t("events.connection")} · {display.mcp_server}
                          </Link>
                        </div>
                      </Card>
                    );
                  })}
                  {!Object.keys(data.config.sources).length && (
                    <Card className="p-10 text-center text-pc-text-secondary">
                      {t("events.no_sources")}
                    </Card>
                  )}
                  <p className="text-xs text-pc-text-muted">
                    {t("events.polling_note")}
                  </p>
                </>
              )}
              {tab === "rules" && (
                <>
                  <div className="flex justify-between gap-4 items-center">
                    <p className="text-sm text-pc-text-secondary">
                      {t("events.rules_hint")}
                    </p>
                    <Button onClick={() => edit("rule")}>
                      <Plus size={15} />
                      {t("events.add_rule")}
                    </Button>
                  </div>
                  {Object.entries(data.config.rules).map(([id, rule]) => (
                    <Card key={id} className="p-5 space-y-4">
                      <div className="flex justify-between">
                        <h2 className="font-semibold text-pc-text">{id}</h2>
                        <Badge
                          tone={
                            rule.enabled &&
                            effectiveSource(rule.source)?.enabled
                              ? "ok"
                              : "neutral"
                          }
                        >
                          {t(
                            rule.enabled &&
                              effectiveSource(rule.source)?.enabled
                              ? "events.enabled"
                              : "events.paused",
                          )}
                        </Badge>
                      </div>
                      <div className="flex flex-wrap items-center gap-3 text-sm">
                        <span className="px-3 py-2 bg-pc-elevated rounded-lg">
                          {rule.source}
                        </span>
                        <ArrowRight size={14} className="text-pc-text-muted" />
                        <span className="text-pc-text-secondary">
                          {effectiveSource(rule.source)?.kind === "fastmail"
                            ? t("events.new_mail")
                            : rule.states.length
                              ? rule.states.join(" / ")
                              : [
                                  rule.below != null ? "< " + rule.below : "",
                                  rule.above != null ? "> " + rule.above : "",
                                ]
                                  .filter(Boolean)
                                  .join(" / ")}
                        </span>
                        <ArrowRight size={14} className="text-pc-text-muted" />
                        <Link
                          to={"/sops/" + encodeURIComponent(rule.sop)}
                          className="text-pc-accent"
                        >
                          {rule.sop}
                        </Link>
                      </div>
                      {effectiveSource(rule.source)?.kind ===
                        "home_assistant" && (
                        <p className="text-xs text-pc-text-secondary">
                          {t("events.duration")} {rule.for_secs}s ·{" "}
                          {t("events.cooldown")} {rule.cooldown_secs}s
                        </p>
                      )}
                      <div className="flex gap-2 border-t border-pc-border pt-3">
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => edit("rule", id, rule)}
                        >
                          {t("events.edit")}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy}
                          onClick={() =>
                            void mutate({
                              ...data.config,
                              rules: {
                                ...data.config.rules,
                                [id]: { ...rule, enabled: !rule.enabled },
                              },
                            })
                          }
                        >
                          {t(rule.enabled ? "events.pause" : "events.enable")}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() =>
                            setTest({
                              rule: id,
                              state: "unavailable",
                              duration: rule.for_secs,
                            })
                          }
                        >
                          {t("events.simulate")}
                        </Button>
                      </div>
                    </Card>
                  ))}
                  {!Object.keys(data.config.rules).length && (
                    <Card className="p-10 text-center text-pc-text-secondary">
                      {t("events.no_rules")}
                    </Card>
                  )}
                  {test && (
                    <Card className="p-5 space-y-3">
                      <div className="flex justify-between">
                        <h3 className="font-medium">
                          {t("events.simulate")} · {test.rule}
                        </h3>
                        <Button
                          variant="ghost"
                          size="sm"
                          aria-label={t("events.close")}
                          onClick={() => setTest(null)}
                        >
                          <X size={14} />
                        </Button>
                      </div>
                      <p className="text-xs text-pc-text-secondary">
                        {t("events.simulate_hint")}
                      </p>
                      {effectiveSource(
                        data.config.rules[test.rule]?.source ?? "",
                      )?.kind === "home_assistant" && (
                        <div className="grid grid-cols-2 gap-3">
                          <Field label={t("events.sample_state")}>
                            <input
                              className={input}
                              value={test.state}
                              onChange={(e) =>
                                setTest({ ...test, state: e.target.value })
                              }
                            />
                          </Field>
                          <Field label={t("events.duration")}>
                            <input
                              className={input}
                              type="number"
                              min="0"
                              value={test.duration}
                              onChange={(e) =>
                                setTest({
                                  ...test,
                                  duration: Number(e.target.value),
                                })
                              }
                            />
                          </Field>
                        </div>
                      )}
                      <Button
                        disabled={busy}
                        onClick={async () => {
                          setBusy(true);
                          try {
                            const r = await simulateEvent(
                              test.rule,
                              test.state,
                              test.duration,
                            );
                            setTest({
                              ...test,
                              result: t("events.result." + r.reason),
                            });
                          } catch (e) {
                            setError(message(e));
                          } finally {
                            setBusy(false);
                          }
                        }}
                      >
                        {t("events.test")}
                      </Button>
                      {test.result && (
                        <p role="status" className="text-sm text-pc-accent">
                          {test.result}
                        </p>
                      )}
                    </Card>
                  )}
                </>
              )}
              {tab === "history" && (
                <>
                  <input
                    aria-label={t("events.search")}
                    className={input}
                    placeholder={t("events.search")}
                    value={filter}
                    onChange={(e) => setFilter(e.target.value)}
                  />
                  {params.get("run") && (
                    <p className="text-xs text-pc-text-secondary">
                      {t("events.for_run")} {params.get("run")} ·{" "}
                      <Link className="text-pc-accent" to="/events/history">
                        {t("events.all")}
                      </Link>
                    </p>
                  )}
                  {!records.length ? (
                    <Card className="p-12 text-center">
                      <Database
                        size={30}
                        className="mx-auto mb-3 text-pc-text-muted"
                      />
                      <h2 className="font-medium text-pc-text">
                        {t("events.no_events")}
                      </h2>
                      <p className="text-sm text-pc-text-secondary mt-2 max-w-lg mx-auto">
                        {t("events.no_events_hint")}
                      </p>
                    </Card>
                  ) : (
                    records.map((r) => (
                      <button
                        key={r.id}
                        onClick={() => setSelectedId(r.id)}
                        className="block w-full text-left rounded-xl border border-pc-border bg-pc-surface p-4 hover:border-pc-border-strong"
                      >
                        <div className="flex justify-between gap-3">
                          <span className="font-medium text-pc-text truncate">
                            {String(
                              r.data.subject ??
                                r.data.entity_id ??
                                r.event_type,
                            )}
                          </span>
                          <Badge
                            tone={
                              r.run_status === "failed" ||
                              r.disposition === "uncertain"
                                ? "warn"
                                : r.notified
                                  ? "ok"
                                  : "neutral"
                            }
                          >
                            {r.notified
                              ? t("events.notified")
                              : r.decision?.notify === false
                                ? t("events.ignored")
                                : t("events.disposition." + r.disposition)}
                          </Badge>
                        </div>
                        <p className="text-xs text-pc-text-secondary mt-2">
                          {r.source} · {r.rule} · {date(r.observed_at_ms)}
                        </p>
                        {r.decision?.reason && (
                          <p className="text-sm text-pc-text-secondary mt-2 line-clamp-2">
                            {r.decision.reason}
                          </p>
                        )}
                      </button>
                    ))
                  )}
                </>
              )}
            </section>
            {editor && (
              <Card className="p-5 space-y-4 self-start sticky top-4">
                <div className="flex justify-between items-center">
                  <h2 className="font-semibold">
                    {t(
                      editor.kind === "source"
                        ? "events.source_settings"
                        : "events.rule_settings",
                    )}
                  </h2>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setEditor(null)}
                    aria-label={t("events.close")}
                  >
                    <X size={15} />
                  </Button>
                </div>
                <form
                  className="space-y-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const key = editor.kind === "source" ? "sources" : "rules";
                    void mutate(
                      {
                        ...data.config,
                        [key]: {
                          ...data.config[key],
                          [editor.id]:
                            editor.kind === "source"
                              ? {
                                  ...editor.value,
                                  entities: parseEntities(
                                    (editor.value as EventSource).entities.join(
                                      "\n",
                                    ),
                                  ),
                                }
                              : {
                                  ...editor.value,
                                  states: parseEntities(
                                    (editor.value as EventRule).states.join(
                                      ",",
                                    ),
                                  ),
                                },
                        },
                      },
                      editor.revision,
                    );
                  }}
                >
                  <Field label={t("events.id")}>
                    <input
                      className={input}
                      required
                      pattern="[A-Za-z0-9_-]{1,64}"
                      disabled={editor.existing}
                      value={editor.id}
                      onChange={(e) =>
                        setEditor({ ...editor, id: e.target.value })
                      }
                    />
                  </Field>
                  {editor.kind === "source" ? (
                    <>
                      <Field label={t("events.type")}>
                        <select
                          className={input}
                          value={(editor.value as EventSource).kind}
                          onChange={(e) =>
                            setEditor({
                              ...editor,
                              value: {
                                ...editor.value,
                                kind: e.target.value,
                                mcp_server: "",
                                plugin: "",
                                entities: [],
                              },
                            })
                          }
                        >
                          <option value="plugin">
                            {t("integrations_plugin.package")}
                          </option>
                          <option value="fastmail">Fastmail</option>
                          <option value="home_assistant">Home Assistant</option>
                        </select>
                      </Field>
                      <Field
                        label={t(
                          (editor.value as EventSource).kind === "plugin"
                            ? "integrations_plugin.instances"
                            : "events.connection",
                        )}
                      >
                        <select
                          className={input}
                          required
                          value={
                            (editor.value as EventSource).kind === "plugin"
                              ? (editor.value as EventSource).plugin
                              : (editor.value as EventSource).mcp_server
                          }
                          onChange={(e) =>
                            field(
                              (editor.value as EventSource).kind === "plugin"
                                ? "plugin"
                                : "mcp_server",
                              e.target.value,
                            )
                          }
                        >
                          <option value="">{t("events.select")}</option>
                          {((editor.value as EventSource).kind === "plugin"
                            ? data.integration_instances
                            : data.connections
                          ).map((id) => (
                            <option key={id}>{id}</option>
                          ))}
                        </select>
                      </Field>
                      <Field
                        label={t(
                          data.resolved_sources[editor.id]?.push
                            ? "events.interval_push"
                            : "events.interval",
                        )}
                      >
                        <input
                          className={input}
                          type="number"
                          min="10"
                          max="3600"
                          required
                          value={
                            (editor.value as EventSource).poll_interval_secs
                          }
                          onChange={(e) =>
                            field("poll_interval_secs", Number(e.target.value))
                          }
                        />
                      </Field>
                      {sourceKind(editor.value as EventSource) !==
                      "home_assistant" ? (
                        <Field label={t("events.query")}>
                          <input
                            className={input}
                            value={(editor.value as EventSource).query}
                            onChange={(e) => field("query", e.target.value)}
                          />
                        </Field>
                      ) : (
                        <Field label={t("events.entities")}>
                          <textarea
                            className={input}
                            rows={4}
                            value={(editor.value as EventSource).entities.join(
                              "\n",
                            )}
                            onChange={(e) =>
                              field("entities", e.target.value.split("\n"))
                            }
                          />
                        </Field>
                      )}
                    </>
                  ) : (
                    <>
                      <Field label={t("events.source")}>
                        <select
                          className={input}
                          required
                          value={(editor.value as EventRule).source}
                          onChange={(e) => field("source", e.target.value)}
                        >
                          <option value="">{t("events.select")}</option>
                          {Object.keys(data.config.sources).map((s) => (
                            <option key={s}>{s}</option>
                          ))}
                        </select>
                      </Field>
                      <Field label={t("events.target_sop")}>
                        <select
                          className={input}
                          required
                          value={(editor.value as EventRule).sop}
                          onChange={(e) => field("sop", e.target.value)}
                        >
                          <option value="">{t("events.select")}</option>
                          {data.sops.map((s) => (
                            <option key={s}>{s}</option>
                          ))}
                        </select>
                      </Field>
                      <p className="text-xs text-pc-text-muted">
                        {t("events.sop_hint")}
                      </p>
                      {effectiveSource((editor.value as EventRule).source)
                        ?.kind === "home_assistant" && (
                        <>
                          <Field label={t("events.states")}>
                            <input
                              className={input}
                              value={(editor.value as EventRule).states.join(
                                ",",
                              )}
                              onChange={(e) =>
                                field("states", e.target.value.split(","))
                              }
                            />
                          </Field>
                          <div className="grid grid-cols-2 gap-3">
                            {["below", "above"].map((k) => (
                              <Field key={k} label={t("events." + k)}>
                                <input
                                  className={input}
                                  type="number"
                                  step="any"
                                  value={
                                    (editor.value as EventRule)[
                                      k as "below" | "above"
                                    ] ?? ""
                                  }
                                  onChange={(e) =>
                                    field(
                                      k,
                                      e.target.value === ""
                                        ? null
                                        : Number(e.target.value),
                                    )
                                  }
                                />
                              </Field>
                            ))}
                          </div>
                          <div className="grid grid-cols-2 gap-3">
                            {["for_secs", "cooldown_secs"].map((k) => (
                              <Field key={k} label={t("events." + k)}>
                                <input
                                  className={input}
                                  type="number"
                                  min="0"
                                  max="86400"
                                  value={
                                    (editor.value as EventRule)[
                                      k as "for_secs" | "cooldown_secs"
                                    ]
                                  }
                                  onChange={(e) =>
                                    field(k, Number(e.target.value))
                                  }
                                />
                              </Field>
                            ))}
                          </div>
                        </>
                      )}
                    </>
                  )}
                  <label className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={editor.value.enabled}
                      onChange={(e) => field("enabled", e.target.checked)}
                    />
                    {t("events.enabled")}
                  </label>
                  <p className="text-xs text-pc-text-muted">
                    {t("events.change_note")}
                  </p>
                  <Button type="submit" disabled={busy || !editor.id}>
                    {t(busy ? "events.saving" : "events.save")}
                  </Button>
                </form>
              </Card>
            )}
            {selected && !editor && (
              <Card className="p-5 space-y-4 self-start sticky top-4">
                <div className="flex justify-between">
                  <h2 className="font-semibold">{t("events.event_detail")}</h2>
                  <Button
                    variant="ghost"
                    size="sm"
                    aria-label={t("events.close")}
                    onClick={() => setSelectedId(null)}
                  >
                    <X size={15} />
                  </Button>
                </div>
                <p className="text-xs break-all text-pc-text-muted">
                  {selected.id}
                </p>
                <p className="text-sm">
                  {date(selected.observed_at_ms)} · {selected.rule}
                </p>
                <pre className="text-xs whitespace-pre-wrap break-all p-3 bg-pc-elevated rounded-lg max-h-72 overflow-auto">
                  {JSON.stringify(selected.data, null, 2)}
                </pre>
                {selected.decision && (
                  <div className="space-y-2 text-sm">
                    <p>{selected.decision.summary}</p>
                    <p className="text-pc-text-secondary">
                      {selected.decision.reason}
                    </p>
                  </div>
                )}
                {selected.run_id && selected.sop && (
                  <Link
                    className="inline-flex gap-2 items-center text-sm text-pc-accent"
                    to={
                      "/runs/" +
                      encodeURIComponent(selected.sop) +
                      "/" +
                      encodeURIComponent(selected.run_id)
                    }
                  >
                    <CheckCircle2 size={15} />
                    {t("events.view_run")} · {selected.run_status ?? "—"}
                  </Link>
                )}
                <p className="text-xs text-pc-text-muted">
                  {t("events.delivery_note")}
                </p>
              </Card>
            )}
          </div>
        </>
      )}
    </div>
  );
}
