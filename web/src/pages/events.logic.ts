export type SourceStatus =
  "paused" | "idle" | "waiting" | "healthy" | "error" | "stale";
export function sourceStatus(
  enabled: boolean,
  interval: number,
  rules: {
    enabled: boolean;
    last_poll_ms?: number | null;
    last_error?: string | null;
  }[],
  now = Date.now(),
): SourceStatus {
  if (!enabled) return "paused";
  const active = rules.filter((r) => r.enabled);
  if (!active.length) return "idle";
  if (active.some((r) => r.last_error)) return "error";
  if (active.some((r) => !r.last_poll_ms)) return "waiting";
  if (
    active.some(
      (r) => now - r.last_poll_ms! > Math.max(120000, interval * 3000),
    )
  )
    return "stale";
  return "healthy";
}
export function parseEntities(value: string): string[] {
  return [
    ...new Set(
      value
        .split(/[\n,]/)
        .map((s) => s.trim())
        .filter(Boolean),
    ),
  ];
}
