import type { EncodeRequest } from "./types";
import type { ExportRecipeSettings } from "./exportRecipes";

export type UserRecipe = {
  id: string;
  name: string;
  settings: ExportRecipeSettings;
};

export type UserRecipeStore = {
  schemaVersion: 2;
  recipes: UserRecipe[];
  warnings: string[];
  migrated: boolean;
  readOnly: boolean;
  sourceSchemaVersion: number | null;
};

export type UserRecipeMutationResult =
  | { ok: true; recipe: UserRecipe; recipes: UserRecipe[] }
  | { ok: false; error: string };

export type UserRecipePersistResult =
  | { ok: true; raw: string; store: UserRecipeStore }
  | { ok: false; error: string };

export const USER_RECIPE_STORAGE_KEY: "vfl:user-recipes";
export const USER_RECIPE_SCHEMA_VERSION: 2;
export const USER_RECIPE_MAX_COUNT: 50;
export const USER_RECIPE_NAME_MAX_LENGTH: 64;

export function normalizeUserRecipeSettings(
  value: unknown,
  options?: { legacyMaxEdgePx?: unknown },
): ExportRecipeSettings | null;
export function reusableSettingsFromEncodeRequest(request: EncodeRequest | unknown): ExportRecipeSettings | null;
export function createEmptyUserRecipeStore(): UserRecipeStore;
export function parseUserRecipeStore(raw: string | null | undefined): UserRecipeStore;
export function loadUserRecipeStore(
  storage: Pick<Storage, "getItem"> | null | undefined,
  key?: string,
): UserRecipeStore;
export function serializeUserRecipeStore(recipes: UserRecipe[]): string;
export function persistUserRecipeStore(
  storage: Pick<Storage, "setItem">,
  currentStore: UserRecipeStore | null | undefined,
  nextRecipes: UserRecipe[],
  key?: string,
): UserRecipePersistResult;
export function generateUserRecipeId(recipes: UserRecipe[], nowMs?: number): string;
export function createUserRecipe(
  recipes: UserRecipe[],
  name: string,
  settings: unknown,
  options?: { id?: string; nowMs?: number },
): UserRecipeMutationResult;
export function renameUserRecipe(recipes: UserRecipe[], id: string, name: string): UserRecipeMutationResult;
export function deleteUserRecipe(recipes: UserRecipe[], id: string): UserRecipeMutationResult;
export function userRecipeMatchesSettings(recipe: UserRecipe | null | undefined, settings: unknown): boolean;
export function findMatchingUserRecipe(recipes: UserRecipe[], settings: unknown): UserRecipe | null;
export function cloneUserRecipeSettings(settings: unknown): ExportRecipeSettings | null;
