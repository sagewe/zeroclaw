import { apiFetch } from "./api";
import type { components } from "./api-generated";
export type IntegrationCatalog =
  components["schemas"]["IntegrationPluginsResponse"];
export type IntegrationConfig =
  components["schemas"]["IntegrationPluginsWrite"]["config"];
export type IntegrationBinding = IntegrationConfig["instances"][string];
export const getIntegrationPlugins = () =>
  apiFetch<IntegrationCatalog>("/api/integration-plugins");
export const saveIntegrationPlugins = (
  config: IntegrationConfig,
  revision: string,
) =>
  apiFetch("/api/integration-plugins", {
    method: "PUT",
    body: JSON.stringify({ config, revision }),
  });
