import type { AdvancedEncodeSettings, InterpolationSettings, OutputFormat } from "./types";

export type PersistedSettings = {
  format?: OutputFormat;
  interpolation?: InterpolationSettings | null;
  normalizeAudio?: boolean;
  stripMetadata?: boolean;
  advanced?: AdvancedEncodeSettings;
};

export function parsePersistedSettings(raw: string | null | undefined): PersistedSettings;
export function serializePersistedSettings(settings: PersistedSettings | null | undefined): string;
