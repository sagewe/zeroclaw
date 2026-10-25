import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import {
  Activity,
  ArrowRight,
  Camera,
  ChevronDown,
  Home as HomeIcon,
  Lightbulb,
  Loader2,
  Plug,
  Power,
  RefreshCw,
  Search,
  ShieldCheck,
  Wifi,
  WifiOff,
  X,
} from "lucide-react";
import { Badge, Button, Card, PageHeader } from "@/components/ui";
import { t } from "@/lib/i18n";
import {
  getHome,
  homeAction,
  type HomeResponse,
  type HomeDevice,
  type HomeEvent,
  type Snapshot,
  type DeviceSpec,
} from "@/lib/home";
import { canSwitch, displayValue, groupRooms } from "./home.logic";

const stamp = (ms?: number | null) =>
  ms ? new Date(ms).toLocaleString() : t("home.never");
export default function Home() {
  const [home, setHome] = useState<HomeResponse | null>(null);
  const [error, setError] = useState("");
  const [loadedAt, setLoadedAt] = useState(0);
  const [clock, setClock] = useState(Date.now());
  const panel = useRef<HTMLElement>(null);
  const [busy, setBusy] = useState("");
  const [notice, setNotice] = useState("");
  const [search, setSearch] = useState("");
  const [tab, setTab] = useState<"devices" | "activity">("devices");
  const [filter, setFilter] = useState<"all" | "control" | "state">("all");
  const [selected, setSelected] = useState<HomeDevice | null>(null);
  const [spec, setSpec] = useState<DeviceSpec | null>(null);
  const [specError, setSpecError] = useState(false);
  const [snapshot, setSnapshot] = useState<Snapshot | null>(null);
  const requestId = useRef(0);
  const actionBusy = useRef(false);
  const mounted = useRef(true);
  const data = home?.data;
  const stale = !!error || !loadedAt || clock - loadedAt > 25000;
  const rooms = useMemo(
    () => groupRooms(data?.devices ?? [], search),
    [data?.devices, search],
  );
  const load = useCallback(async (signal?: AbortSignal) => {
    const id = ++requestId.current;
    try {
      const value = await getHome(signal);
      if (mounted.current && id === requestId.current) {
        setHome(value);
        setLoadedAt(Date.now());
        setError("");
      }
    } catch (e) {
      if (!signal?.aborted && mounted.current && id === requestId.current)
        setError(t("home.load_error"));
    }
  }, []);
  useEffect(() => {
    const timer = setInterval(() => setClock(Date.now()), 5000);
    return () => clearInterval(timer);
  }, []);
  useEffect(() => {
    mounted.current = true;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      if (!document.hidden) await load(controller.signal);
      if (!controller.signal.aborted) timer = setTimeout(poll, 10000);
    };
    void poll();
    return () => {
      mounted.current = false;
      controller.abort();
      clearTimeout(timer);
      ++requestId.current;
    };
  }, [load]);
  useEffect(() => {
    setSpec(null);
    setSpecError(false);
    if (!selected) return;
    let alive = true;
    homeAction<DeviceSpec>({ action: "device_spec", device_id: selected.did })
      .then((v) => {
        if (alive) setSpec(v);
      })
      .catch(() => {
        if (alive) setSpecError(true);
      });
    return () => {
      alive = false;
    };
  }, [selected]);
  useEffect(() => {
    if (!selected) return;
    const previous = document.activeElement as HTMLElement | null;
    panel.current?.querySelector<HTMLButtonElement>("button")?.focus();
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSelected(null);
      if (e.key === "Tab" && panel.current) {
        const nodes = [
          ...panel.current.querySelectorAll<HTMLElement>(
            'button:not(:disabled), summary, [tabindex="0"]',
          ),
        ];
        const first = nodes[0],
          last = nodes[nodes.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last?.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first?.focus();
        }
      }
    };
    window.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("keydown", close);
      if (previous?.isConnected) previous.focus();
    };
  }, [selected]);
  const perform = async (key: string, work: () => Promise<void>) => {
    if (actionBusy.current) return;
    actionBusy.current = true;
    setBusy(key);
    setNotice("");
    try {
      await work();
      if (mounted.current) await load();
    } catch {
      if (mounted.current)
        setNotice(
          t(key === "snapshot" ? "home.capture_error" : "home.action_error"),
        );
    } finally {
      actionBusy.current = false;
      if (mounted.current) setBusy("");
    }
  };
  const events = (data?.events ?? []).filter(
    (e) =>
      filter === "all" ||
      (filter === "control") === (e.kind === "control_completed"),
  );
  const deviceMap = new Map(data?.devices.map((d) => [d.did, d]));
  const activeDevice = selected && (deviceMap.get(selected.did) ?? selected);
  const summary = (event: HomeEvent) => {
    if (event.kind === "online_changed")
      return t(event.data.online ? "home.came_online" : "home.went_offline");
    if (event.kind === "control_completed")
      return `${event.data.light ?? ""} · ${t("home.state." + event.data.requested)} · ${t(event.data.success ? "home.confirmed" : "home.unconfirmed")}${event.data.result?.changed === false ? " · " + t("home.no_change") : ""}`;
    if (typeof event.data.value === "object" || (typeof event.data.value === "string" && event.data.value.length > 80)) return t("home.report_updated");
    return `${event.data.label ?? t("home.property_changed")} · ${displayValue(event.data.old)} → ${displayValue(event.data.value)}`;
  };
  return (
    <div className="p-4 md:p-6 space-y-6 max-w-7xl mx-auto">
      <PageHeader
        title={t("home.title")}
        description={t("home.description")}
        actions={
          <>
            <Link
              to="/plugins/integrations"
              className="text-sm text-pc-text-secondary hover:text-pc-text"
            >
              {t("home.manage")}
            </Link>
            <Button
              variant="ghost"
              disabled={!!busy || !home}
              onClick={() =>
                void perform("refresh", async () => {
                  await homeAction({ action: "refresh" });
                })
              }
            >
              <RefreshCw
                size={15}
                className={busy === "refresh" ? "animate-spin" : ""}
              />
              {t("home.sync")}
            </Button>
          </>
        }
      />
      {error && (
        <div
          role="alert"
          className="rounded-lg border border-status-warning/30 p-4 text-status-warning flex items-center justify-between gap-3"
        >
          <span>{error}</span>
          <Button variant="ghost" onClick={() => void load()}>
            {t("home.retry")}
          </Button>
        </div>
      )}
      {notice && (
        <div
          role="status"
          className="rounded-lg bg-pc-surface border border-pc-border p-3 text-sm"
        >
          {notice}
        </div>
      )}
      {!data && !error && (
        <div className="flex items-center gap-2 py-20 justify-center text-pc-text-muted">
          <Loader2 className="animate-spin" size={20} />
          {t("home.loading")}
        </div>
      )}
      {data && (
        <>
          <div className="grid grid-cols-2 lg:grid-cols-4 gap-3">
            <Metric
              label={t("home.connection")}
              value={
                stale
                  ? t("home.stale")
                  : data.health.ready
                    ? t("home.connected")
                    : t("home.reconnecting")
              }
              icon={
                data.health.ready && !stale ? (
                  <Wifi size={18} />
                ) : (
                  <WifiOff size={18} />
                )
              }
              detail={`clowder ${data.health.version}`}
            />
            <Metric
              label={t("home.devices")}
              value={String(data.devices.length)}
              icon={<Plug size={18} />}
              detail={`${data.devices.filter((d) => d.online).length} ${t("home.online")} · ${data.devices.filter((d) => !d.online).length} ${t("home.offline")}`}
            />
            <Metric
              label={t("home.control_scope")}
              value={`${data.lights.length} ${t("home.lights")}`}
              icon={<ShieldCheck size={18} />}
              detail={
                home.can_control
                  ? t("home.scoped_controls")
                  : t("home.read_only")
              }
            />
            <Metric
              label={t("home.last_push")}
              value={
                data.health.last_push_ms
                  ? new Date(data.health.last_push_ms).toLocaleTimeString()
                  : t("home.never")
              }
              icon={<Activity size={18} />}
              detail={`${data.health.subscribed_devices} ${t("home.subscribed")}`}
            />
          </div>
          <div className="grid lg:grid-cols-[minmax(0,1fr)_320px] gap-5 items-start">
            <div className="space-y-4 min-w-0 order-2 lg:order-1">
              <div
                className="flex items-center gap-5 border-b border-pc-border"
                role="tablist"
                aria-label={t("home.views")}
              >
                {(["devices", "activity"] as const).map((v) => (
                  <button
                    role="tab"
                    aria-selected={tab === v}
                    key={v}
                    onClick={() => setTab(v)}
                    className={`pb-3 text-sm border-b-2 ${tab === v ? "border-pc-accent text-pc-text" : "border-transparent text-pc-text-muted"}`}
                  >
                    {t("home." + v)}
                  </button>
                ))}
              </div>
              {tab === "devices" ? (
                <>
                  <label className="flex items-center gap-2 rounded-lg border border-pc-border px-3 py-2 bg-pc-surface">
                    <Search size={16} className="text-pc-text-muted" />
                    <input
                      value={search}
                      onChange={(e) => setSearch(e.target.value)}
                      placeholder={t("home.search")}
                      aria-label={t("home.search")}
                      className="w-full bg-transparent text-sm outline-none"
                    />
                  </label>
                  {!rooms.length && (
                    <Card className="py-12 text-center text-pc-text-muted">
                      {t("home.no_devices")}
                    </Card>
                  )}
                  {rooms.map(([key, room]) => (
                    <details
                      key={key}
                      open
                      className="group rounded-xl border border-pc-border bg-pc-surface overflow-hidden"
                    >
                      <summary className="list-none cursor-pointer flex items-center gap-2 p-4 border-b border-pc-border">
                        <HomeIcon size={16} className="text-pc-text-muted" />
                        <span className="font-medium flex-1">
                          {room.name || t("home.unassigned")}
                        </span>
                        <span className="text-xs text-pc-text-muted">
                          {room.devices.length} {t("home.device_unit")}
                        </span>
                        <ChevronDown size={15} />
                      </summary>
                      <div className="divide-y divide-pc-border">
                        {room.devices.map((device) => {
                          const lights = data.lights.filter(
                            (l) => l.device_id === device.did,
                          );
                          const Icon = device.model.includes("camera")
                            ? Camera
                            : device.model.includes("light")
                              ? Lightbulb
                              : lights.length ? Power : Plug;
                          return (
                            <div key={device.did} className="px-4 py-3">
                              <button
                                onClick={() => setSelected(device)}
                                className="flex items-center gap-3 w-full text-left hover:text-pc-accent"
                              >
                                <span className="w-9 h-9 bg-pc-elevated rounded-lg grid place-items-center text-pc-text-secondary">
                                  <Icon size={17} />
                                </span>
                                <span className="flex-1 min-w-0">
                                  <span className="text-sm font-medium block truncate">
                                    {device.name}
                                  </span>
                                  <span className="text-xs text-pc-text-muted block truncate">
                                    {device.model}
                                  </span>
                                </span>
                                <span
                                  className={`text-xs inline-flex items-center gap-1.5 ${stale || !data.health.ready ? "text-status-warning" : device.online ? "text-status-success" : "text-pc-text-muted"}`}
                                >
                                  <span className="w-1.5 h-1.5 rounded-full bg-current" />
                                  {t(
                                    stale || !data.health.ready
                                      ? "home.stale"
                                      : device.online
                                        ? "home.online"
                                        : "home.offline",
                                  )}
                                </span>
                                <ArrowRight size={14} />
                              </button>
                              {lights.length > 0 && (
                                <div className="mt-3 ml-12 space-y-2">
                                  {lights.map((light) => (
                                    <div
                                      key={light.entity_id}
                                      className="flex justify-between items-center gap-2 text-sm"
                                    >
                                      <span className="text-pc-text-secondary">
                                        {light.light}
                                      </span>
                                      <div className="flex gap-2 items-center">
                                        <span className="text-xs text-pc-text-muted">
                                          {t("home.state." + light.state)}
                                        </span>
                                        <button
                                          role="switch"
                                          aria-checked={light.state === "on"}
                                          aria-label={light.light}
                                          disabled={
                                            !!busy ||
                                            !canSwitch(
                                              light,
                                              home.can_control,
                                              data.health.ready,
                                              stale,
                                            )
                                          }
                                          onClick={() =>
                                            void perform(
                                              light.entity_id,
                                              async () => {
                                                if (
                                                  Date.now() - loadedAt >
                                                  25000
                                                )
                                                  throw new Error("stale");
                                                await homeAction({
                                                  action: "set_power",
                                                  light: light.light,
                                                  state:
                                                    light.state === "on"
                                                      ? "off"
                                                      : "on",
                                                });
                                                setNotice(t("home.confirmed"));
                                              },
                                            )
                                          }
                                          className={`relative w-9 h-5 rounded-full transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${light.state === "on" ? "bg-pc-accent" : "bg-pc-border-strong"}`}
                                        >
                                          <span
                                            className={`absolute top-0.5 w-4 h-4 bg-white rounded-full transition-transform ${light.state === "on" ? "left-0.5 translate-x-4" : "left-0.5"}`}
                                          />
                                        </button>
                                      </div>
                                    </div>
                                  ))}
                                </div>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    </details>
                  ))}
                </>
              ) : (
                <>
                  <div className="flex justify-between gap-3 flex-wrap">
                    <p className="text-xs text-pc-text-muted">
                      {t("home.activity_hint")}
                    </p>
                    <select
                      aria-label={t("home.filter")}
                      value={filter}
                      onChange={(e) =>
                        setFilter(e.target.value as typeof filter)
                      }
                      className="bg-pc-surface border border-pc-border rounded px-2 text-sm"
                    >
                      <option value="all">{t("home.all")}</option>
                      <option value="state">{t("home.state_changes")}</option>
                      <option value="control">{t("home.controls")}</option>
                    </select>
                  </div>
                  <Card padded={false}>
                    <ol className="divide-y divide-pc-border">
                      {events.map((e) => (
                        <li key={e.id} className="p-4 flex gap-3">
                          <span
                            className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${e.kind === "control_completed" ? (e.data.success ? "bg-status-success" : "bg-status-error") : "bg-pc-accent"}`}
                          />
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap gap-x-3 gap-y-1 justify-between">
                              <button className="text-sm font-medium text-left hover:text-pc-accent" onClick={() => setSelected(deviceMap.get(e.device_id) ?? null)}>
                                {deviceMap.get(e.device_id)?.name ?? e.device_id}
                              </button>
                              <time className="text-xs text-pc-text-muted">
                                {stamp(e.received_at_ms)}
                              </time>
                            </div>
                            <p className="text-sm text-pc-text-secondary mt-1 break-words">
                              {summary(e)}
                            </p>
                            <span className="text-xs text-pc-text-muted">
                              {deviceMap.get(e.device_id)?.room_name}
                            </span>
                            {e.kind === "property_changed" && <details className="text-xs text-pc-text-muted mt-2"><summary className="cursor-pointer">{t("home.event_details")}</summary><p className="mt-2 font-mono break-all">{e.data.siid}.{e.data.piid} · {displayValue(e.data.old)} → {displayValue(e.data.value)}</p></details>}
                            {e.data.error && (
                              <p className="text-xs text-status-error mt-1 break-all">
                                {e.data.error}
                              </p>
                            )}
                          </div>
                        </li>
                      ))}
                    </ol>
                    {!events.length && (
                      <p className="p-10 text-center text-pc-text-muted">
                        {t("home.no_activity")}
                      </p>
                    )}
                  </Card>
                  <Link
                    to="/events/history"
                    className="text-sm text-pc-accent inline-flex gap-1 items-center"
                  >
                    {t("home.agent_events")}
                    <ArrowRight size={14} />
                  </Link>
                </>
              )}
            </div>
            <aside className="space-y-4 order-1 lg:order-2">
              <Card>
                <div className="flex items-center justify-between mb-3">
                  <h2 className="font-medium flex items-center gap-2">
                    <Camera size={17} />
                    {t("home.camera")}
                  </h2>
                  <Badge tone="neutral">{t("home.on_demand")}</Badge>
                </div>
                <p className="text-xs text-pc-text-muted mb-4">
                  {t("home.camera_hint")}
                </p>
                <div className="grid grid-cols-2 lg:grid-cols-1 gap-3">
                  {[0, 1].map((i) => (
                    <div key={i}>
                      <div className="aspect-video bg-pc-elevated rounded-lg overflow-hidden flex items-center justify-center">
                        {snapshot?.images[i] ? (
                          <img
                            className="w-full h-full object-contain"
                            src={`data:image/jpeg;base64,${snapshot.images[i].data}`}
                            alt={`${t("home.lens")} ${i + 1}`}
                          />
                        ) : (
                          <div className="text-pc-text-muted text-xs flex flex-col items-center gap-2">
                            <Camera size={24} />
                            {t("home.no_snapshot")}
                          </div>
                        )}
                      </div>
                      <p className="text-xs text-pc-text-muted mt-1">
                        {t("home.lens")} {i + 1}
                      </p>
                    </div>
                  ))}
                </div>
                {snapshot && (
                  <p className="text-xs text-pc-text-secondary my-3">
                    {t("home.captured")} {stamp(snapshot.captured_at_ms)}
                  </p>
                )}
                <Button
                  className="w-full mt-3"
                  disabled={!!busy || !home.can_capture}
                  onClick={() =>
                    void perform("snapshot", async () => {
                      const shot = await homeAction<Snapshot>({
                        action: "snapshot",
                      });
                      if (mounted.current) setSnapshot(shot);
                    })
                  }
                >
                  {busy === "snapshot" ? (
                    <Loader2 size={16} className="animate-spin" />
                  ) : (
                    <Camera size={16} />
                  )}{" "}
                  {t(busy === "snapshot" ? "home.capturing" : "home.capture")}
                </Button>
                {!home.can_capture && (
                  <p className="mt-2 text-xs text-status-warning">
                    {t("home.capture_disabled")}
                  </p>
                )}
              </Card>
              <Card>
                <h2 className="font-medium text-sm mb-2">
                  {t("home.observation")}
                </h2>
                <p className="text-xs text-pc-text-muted leading-relaxed">
                  {t("home.observation_hint")}
                </p>
                <dl className="text-xs mt-4 space-y-2">
                  <div className="flex justify-between gap-2">
                    <dt className="text-pc-text-muted">{t("home.updated")}</dt>
                    <dd>
                      {loadedAt ? new Date(loadedAt).toLocaleTimeString() : "—"}
                    </dd>
                  </div>
                  <div className="flex justify-between">
                    <dt className="text-pc-text-muted">{t("home.agent")}</dt>
                    <dd>{home.agent}</dd>
                  </div>
                </dl>
                <Link
                  to="/events/rules"
                  className="text-sm mt-4 text-pc-accent inline-flex items-center gap-1"
                >
                  {t("home.rules")}
                  <ArrowRight size={14} />
                </Link>
              </Card>
            </aside>
          </div>
        </>
      )}
      {activeDevice && (
        <div
          className="fixed inset-0 z-50 bg-black/40 flex justify-end"
          onClick={() => setSelected(null)}
        >
          <section
            ref={panel}
            role="dialog"
            aria-modal="true"
            aria-label={activeDevice.name}
            className="w-full max-w-lg h-full bg-pc-base shadow-xl overflow-y-auto p-6 space-y-5"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between gap-3">
              <h2 className="text-lg font-semibold">{activeDevice.name}</h2>
              <button
                aria-label={t("home.close")}
                onClick={() => setSelected(null)}
              >
                <X size={20} />
              </button>
            </div>
            <p className="text-sm text-pc-text-muted">
              {activeDevice.room_name} · {activeDevice.model}
            </p>
            <p className="text-xs text-pc-text-muted">
              {t("home.reported")} {stamp(activeDevice.reported_at_ms)}
            </p>
            <h3 className="font-medium">{t("home.properties")}</h3>
            <p className="text-xs text-pc-text-muted">
              {t("home.properties_hint")}
            </p>
            {(data?.properties ?? [])
              .filter((p) => p.device_id === activeDevice.did)
              .map((p) => {
                const service = spec?.services.find((s) => s.iid === p.siid);
                const prop = service?.properties?.find((v) => v.iid === p.piid);
                return (
                  <Card key={`${p.siid}.${p.piid}`} className="space-y-1">
                    <div className="flex justify-between gap-4">
                      <span className="text-sm">
                        {p.label || prop?.description || `${p.siid}.${p.piid}`}
                      </span>
                      <span className="font-mono text-sm break-all">
                        {prop?.["value-list"]?.find((v) => v.value === p.value)
                          ?.description ?? displayValue(p.value)}{" "}
                        {prop?.unit === "none" ? "" : prop?.unit}
                      </span>
                    </div>
                    <p className="text-xs text-pc-text-muted">
                      {stamp(p.reported_at_ms)} ·{" "}
                      {p.source === "mqtt_push"
                        ? t("home.push")
                        : t("home.read")}
                    </p>
                  </Card>
                );
              })}
            {!data?.properties.some(
              (p) => p.device_id === activeDevice.did,
            ) && (
              <p className="text-sm text-pc-text-muted">
                {t("home.no_properties")}
              </p>
            )}
            <details className="border-t border-pc-border pt-4">
              <summary className="cursor-pointer text-sm">
                {t("home.capabilities")}
              </summary>
              <p className="text-xs text-pc-text-muted mt-2">{t("home.capabilities_hint")}</p>
              {specError ? (
                <p className="text-status-warning text-sm mt-3">
                  {t("home.spec_error")}
                </p>
              ) : !spec ? (
                <Loader2 className="animate-spin mt-3" size={16} />
              ) : (
                spec.services.map((s) => (
                  <div key={s.iid} className="mt-3">
                    <h4 className="text-sm font-medium">
                      {s.description || String(s.iid)}
                    </h4>
                    <ul className="text-xs text-pc-text-muted space-y-1 mt-1">
                      {s.properties?.map((p) => (
                        <li key={p.iid}>
                          {s.iid}.{p.iid} · {p.description} ·{" "}
                          {p.access?.join(" / ")}
                        </li>
                      ))}
                    </ul>
                  </div>
                ))
              )}
            </details>
          </section>
        </div>
      )}
    </div>
  );
}
function Metric({
  label,
  value,
  detail,
  icon,
}: {
  label: string;
  value: string;
  detail: string;
  icon: React.ReactNode;
}) {
  return (
    <Card>
      <div className="flex justify-between gap-2 text-xs text-pc-text-muted">
        <span>{label}</span>
        {icon}
      </div>
      <div className="text-xl font-semibold mt-2 mb-1 truncate">{value}</div>
      <p className="text-xs text-pc-text-muted truncate">{detail}</p>
    </Card>
  );
}
