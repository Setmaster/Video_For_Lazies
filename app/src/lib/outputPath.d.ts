export type SplitPath = { dir: string; base: string };

export function splitPath(p: string): SplitPath;
export function basename(p: string): string;
export function dirname(p: string): string;
export function extname(p: string): string;
export function stem(p: string): string;
export function replaceExtension(p: string, newExt: string): string;
export function suggestOutputPath(inputPath: string, formatExt: string): string;

