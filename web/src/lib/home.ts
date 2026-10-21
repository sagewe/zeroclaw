import { apiFetch } from "./api";
import type { components } from "./api-generated";
export interface HomeDevice {
  did: string;
  name: string;
  model: string;
  room_id: string;
  room_name: string;
  online: boolean;
  reported_at_ms: number;
  urn: string;
}
export interface HomeLight {
  light: string;
  device_id: string;
  entity_id: string;
  state: string;
}
export interface HomeProperty {
  device_id: string;
  siid: number;
  piid: number;
  value: unknown;
  source: string;
  reported_at_ms: number;
}
export interface HomeEvent {
  id: number;
  kind: string;
  device_id: string;
  data: {
    online?: boolean;
    siid?: number;
    piid?: number;
    old?: unknown;
    value?: unknown;
    light?: string;
    requested?: string;
    success?: boolean;
    error?: string;
    result?: { changed?: boolean; confirmed?: boolean };
  };
  received_at_ms: number;
}
export interface HomeData {
  health: {
    ready: boolean;
    mqtt_connected: boolean;
    subscribed_devices: number;
    last_push_ms: number | null;
    last_error: string | null;
    version: string;
  };
  devices: HomeDevice[];
  lights: HomeLight[];
  properties: HomeProperty[];
  events: HomeEvent[];
  observed_at_ms: number;
}
export type HomeResponse = Omit<
  components["schemas"]["HomeResponse"],
  "data"
> & { data: HomeData };
export type HomeAction = components["schemas"]["HomeAction"];
export interface Snapshot {
  images: { mime_type: string; data: string }[];
  captured_at_ms: number;
}
export interface DeviceSpec {
  services: {
    iid: number;
    description?: string;
    properties?: {
      iid: number;
      description?: string;
      unit?: string;
      access?: string[];
      "value-list"?: { value: unknown; description: string }[];
    }[];
  }[];
}
export const getHome = (signal?: AbortSignal) =>
  apiFetch<HomeResponse>("/api/home", { signal });
export const homeAction = <T = unknown>(action: HomeAction) =>
  apiFetch<T>("/api/home/action", {
    method: "POST",
    body: JSON.stringify(action),
  });
