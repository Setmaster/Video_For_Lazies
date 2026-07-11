import type {
  AudioChannelPreference,
  EncodeSpeedPreference,
  OutputFormat,
  ResizeMode,
  VideoCodecPreference,
  VideoQualityPreference,
} from "./types";

export type ExportRecipeResizeSettings = {
  mode: ResizeMode;
  maxEdgePx: string;
  widthPx: string;
  heightPx: string;
  lockAspect: boolean;
};

export type ExportRecipeSettings = {
  format: OutputFormat;
  sizeLimitMb: string;
  resize: ExportRecipeResizeSettings;
  maxEdgePx?: string;
  audioEnabled: boolean;
  normalizeAudio?: boolean;
  perturbFirstFrame?: boolean;
  strictFit: boolean;
  strictFitAllowAudioRemoval: boolean;
  advanced: {
    videoCodec: VideoCodecPreference;
    audioBitrateKbps: number | null;
    videoQuality: VideoQualityPreference;
    encodeSpeed: EncodeSpeedPreference;
    frameRateCapFps: number | null;
    audioChannels: AudioChannelPreference;
  };
};

export type PartialExportRecipeSettings = Partial<
  Pick<ExportRecipeSettings, "format" | "sizeLimitMb" | "audioEnabled" | "normalizeAudio" | "perturbFirstFrame" | "strictFit" | "strictFitAllowAudioRemoval">
>;

export type ExportRecipe =
  | {
      id: string;
      label: string;
      description: string;
      partial?: false;
      settings: ExportRecipeSettings;
    }
  | {
      id: string;
      label: string;
      description: string;
      // Partial recipes only set (and only match) the settings they list.
      partial: true;
      settings: PartialExportRecipeSettings;
    };

export const EXPORT_RECIPES: ExportRecipe[];

export function findExportRecipe(id: string): ExportRecipe | null;
export function normalizeRecipeResizeSettings(
  settings: ExportRecipeSettings | (Partial<ExportRecipeSettings> & { resize?: Partial<ExportRecipeResizeSettings> }) | null | undefined,
): ExportRecipeResizeSettings;
export function recipeMatchesSettings(
  recipe: ExportRecipe | null | undefined,
  settings: ExportRecipeSettings | (Partial<ExportRecipeSettings> & { resize?: Partial<ExportRecipeResizeSettings> }) | null | undefined,
): boolean;
export function findMatchingExportRecipe(
  settings: ExportRecipeSettings | (Partial<ExportRecipeSettings> & { resize?: Partial<ExportRecipeResizeSettings> }) | null | undefined,
): ExportRecipe | null;
