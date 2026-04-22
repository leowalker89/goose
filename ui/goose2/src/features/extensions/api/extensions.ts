import { getClient } from "@/shared/api/acpConnection";
import type {
  ExtensionConfig,
  ExtensionEntry,
  ExtensionStatusEntry,
} from "../types";

const EXTENSIONS_CONFIG_KEY = "extensions";

export function nameToKey(name: string): string {
  return name
    .replace(/\s/g, "")
    .replace(/[^a-zA-Z0-9_-]/g, "_")
    .toLowerCase();
}

function toExtensionsConfigMap(value: unknown): Record<string, unknown> {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return { ...(value as Record<string, unknown>) };
  }
  return {};
}

async function updateExtensionsConfig(
  updater: (extensions: Record<string, unknown>) => Record<string, unknown>,
): Promise<void> {
  const client = await getClient();
  const current = await client.goose.GooseConfigRead({
    key: EXTENSIONS_CONFIG_KEY,
  });
  const next = updater(toExtensionsConfigMap(current.value));
  await client.goose.GooseConfigUpsert({
    key: EXTENSIONS_CONFIG_KEY,
    value: next,
  });
}

export async function listExtensions(): Promise<ExtensionEntry[]> {
  const client = await getClient();
  const response = await client.goose.GooseConfigExtensions({});
  return (response.extensions ?? []) as ExtensionEntry[];
}

export async function listSessionExtensionStatuses(
  sessionId: string,
): Promise<ExtensionStatusEntry[]> {
  const client = await getClient();
  const response = await client.goose.GooseSessionExtensionsStatuses({
    sessionId,
  });
  return (response.extensions ?? []) as ExtensionStatusEntry[];
}

export async function addExtension(
  name: string,
  extensionConfig: ExtensionConfig,
  enabled: boolean,
): Promise<void> {
  const configKey = nameToKey(name);
  return updateExtensionsConfig((extensions) => ({
    ...extensions,
    [configKey]: {
      ...extensionConfig,
      enabled,
      name,
    },
  }));
}

export async function removeExtension(configKey: string): Promise<void> {
  return updateExtensionsConfig((extensions) => {
    const next = { ...extensions };
    delete next[configKey];
    return next;
  });
}

export async function toggleExtension(
  configKey: string,
  enabled: boolean,
): Promise<void> {
  return updateExtensionsConfig((extensions) => {
    const entry = extensions[configKey];
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      throw new Error(`Extension '${configKey}' not found`);
    }
    return {
      ...extensions,
      [configKey]: {
        ...entry,
        enabled,
      },
    };
  });
}
