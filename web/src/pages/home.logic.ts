import type { HomeDevice, HomeLight } from "../lib/home";
export function groupRooms(devices: HomeDevice[], search: string) {
  const groups = new Map<string, { name: string; devices: HomeDevice[] }>();
  const query = search.trim().toLocaleLowerCase();
  for (const device of devices) {
    if (
      query &&
      !`${device.name} ${device.room_name} ${device.model}`
        .toLocaleLowerCase()
        .includes(query)
    )
      continue;
    const key = device.room_id || "";
    const group = groups.get(key) ?? { name: device.room_name, devices: [] };
    group.devices.push(device);
    groups.set(key, group);
  }
  return [...groups.entries()].sort((a, b) =>
    a[1].name.localeCompare(b[1].name),
  );
}
export function canSwitch(
  light: HomeLight,
  allowed: boolean,
  ready: boolean,
  stale: boolean,
) {
  return (
    allowed &&
    ready &&
    !stale &&
    (light.state === "on" || light.state === "off")
  );
}
export function displayValue(value: unknown): string {
  if (value === null || value === undefined) return "—";
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return text.length > 180 ? text.slice(0, 180) + "…" : text;
}
