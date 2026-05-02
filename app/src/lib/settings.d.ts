import type { OutputFormat } from "./types";

export type PersistedSettings = {
  format?: OutputFormat;
};

export function parsePersistedSettings(raw: string | null | undefined): PersistedSettings;
export function serializePersistedSettings(settings: PersistedSettings | null | undefined): string;
