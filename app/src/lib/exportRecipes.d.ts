import type {
  AudioChannelPreference,
  EncodeSpeedPreference,
  OutputFormat,
  VideoCodecPreference,
  VideoQualityPreference,
} from "./types";

export type ExportRecipeSettings = {
  format: OutputFormat;
  sizeLimitMb: string;
  maxEdgePx: string;
  audioEnabled: boolean;
  advanced: {
    videoCodec: VideoCodecPreference;
    audioBitrateKbps: number | null;
    videoQuality: VideoQualityPreference;
    encodeSpeed: EncodeSpeedPreference;
    frameRateCapFps: number | null;
    audioChannels: AudioChannelPreference;
  };
};

export type ExportRecipe = {
  id: string;
  label: string;
  description: string;
  settings: ExportRecipeSettings;
};

export const EXPORT_RECIPES: ExportRecipe[];

export function findExportRecipe(id: string): ExportRecipe | null;
export function recipeMatchesSettings(
  recipe: ExportRecipe | null | undefined,
  settings: ExportRecipeSettings | null | undefined,
): boolean;
export function findMatchingExportRecipe(settings: ExportRecipeSettings | null | undefined): ExportRecipe | null;
