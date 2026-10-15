import { apiFetch } from "./api";
import type { components } from "./api-generated";
export type EventsConfig = components["schemas"]["EventsWrite"]["config"];
export type EventSource = EventsConfig["sources"][string];
export type EventRule = EventsConfig["rules"][string];
export interface EventRecord {
  id: string;
  rule: string;
  source: string;
  sop: string | null;
  observed_at_ms: number;
  event_type: string;
  disposition: string;
  run_id?: string | null;
  run_status?: string;
  notified?: boolean;
  data: Record<string, unknown>;
  decision?: { notify?: boolean; reason?: string; summary?: string };
}
export interface RuleHealth {
  id: string;
  last_poll_ms?: number | null;
  last_error?: string | null;
}
export type EventsState = Omit<
  components["schemas"]["EventsResponse"],
  "rules" | "records" | "resolved_sources"
> & {
  rules: RuleHealth[];
  records: EventRecord[];
  resolved_sources: Record<string, {
    kind: string;
    enabled: boolean;
    mcp_server: string;
    valid: boolean;
  }>;
};
export const getEvents = () => apiFetch<EventsState>("/api/event-monitor");
export const saveEvents = (config: EventsConfig, revision: string) =>
  apiFetch("/api/event-monitor", {
    method: "PUT",
    body: JSON.stringify({ config, revision }),
  });
export const simulateEvent = (
  rule: string,
  state: string,
  duration_secs: number,
) =>
  apiFetch<{ matched: boolean; reason: string; sop: string }>(
    "/api/event-monitor/simulate",
    { method: "POST", body: JSON.stringify({ rule, state, duration_secs }) },
  );
