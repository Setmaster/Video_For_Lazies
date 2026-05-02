import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("trim UI keeps preview synced to the selected start or end handle", async () => {
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const cssPath = path.resolve(__dirname, "../src/App.css");
  const raw = await fs.readFile(appPath, "utf8");
  const css = await fs.readFile(cssPath, "utf8");

  assert.match(raw, /const \[activeTrimTarget, setActiveTrimTarget\] = useState<TrimFocusTarget>\("preview"\);/);
  assert.match(raw, /function focusTrimTarget\(target: Exclude<TrimFocusTarget, "preview">\)/);
  assert.match(raw, /function beginTrimHandleDrag\(target: Exclude<TrimFocusTarget, "preview">, event: ReactPointerEvent<HTMLButtonElement>\)/);
  assert.match(raw, /syncPreviewToTime\(nextTimeS, target, \{ pause: true \}\);/);
  assert.match(raw, /onPointerDown=\{\(e\) => beginTrimHandleDrag\("start", e\)\}/);
  assert.match(raw, /onPointerDown=\{\(e\) => beginTrimHandleDrag\("end", e\)\}/);
  assert.match(raw, /Drag either trim handle or click Start\/End above to make the preview jump to that boundary\./);
  assert.match(raw, /ref=\{trimTimelineTrackRef\}/);
  assert.match(css, /\.vfl-trim-timeline-grab \{/);
  assert.match(css, /\.vfl-trim-timeline-grab:hover:enabled \{\s*transform: translate\(-50%, -50%\) scale\(1\.04\);/s);
  assert.match(css, /\.vfl-trim-timeline-grab:active:enabled \{\s*transform: translate\(-50%, -50%\) scale\(0\.98\);/s);
  assert.doesNotMatch(raw, /vfl-trim-timeline-input-start/);
});

test("set start and set end use the remembered preview selection instead of the active trim handle focus", async () => {
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const raw = await fs.readFile(appPath, "utf8");

  assert.match(raw, /const previewSelectionTimeRef = useRef\(0\);/);
  assert.match(raw, /if \(activeTrimTarget === "preview" \|\| previewPlaying\) \{\s*previewSelectionTimeRef\.current = previewTimeS;\s*\}/s);
  assert.match(raw, /if \(target === "preview"\) \{\s*previewSelectionTimeRef\.current = safeTimeS;\s*\}/s);
  assert.match(raw, /function applyTrimStartFromCurrent\(\) \{\s*updateTrimTarget\("start", previewSelectionTimeRef\.current, \{ pause: true \}\);\s*\}/s);
  assert.match(raw, /function applyTrimEndFromCurrent\(\) \{\s*updateTrimTarget\("end", previewSelectionTimeRef\.current, \{ pause: true \}\);\s*\}/s);
  assert.match(raw, /function focusTrimTarget\(target: Exclude<TrimFocusTarget, "preview">\) \{\s*if \(!trimTimeline\) return;\s*const nextTimeS = target === "start" \? trimTimeline\.start : trimTimeline\.end;\s*syncPreviewToTime\(nextTimeS, target, \{ pause: true \}\);\s*\}/s);
});

test("trim UI exposes drag snap controls and compose keyboard shortcuts", async () => {
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const cssPath = path.resolve(__dirname, "../src/App.css");
  const raw = await fs.readFile(appPath, "utf8");
  const css = await fs.readFile(cssPath, "utf8");

  assert.match(raw, /const TRIM_DRAG_SNAP_MAX_S = 60;/);
  assert.match(raw, /const TRIM_FINE_NUDGE_S = 0\.1;/);
  assert.match(raw, /const TRIM_COARSE_NUDGE_S = 1;/);
  assert.match(raw, /function normalizeTrimDragSnapInput\(rawValue: string, durationS: number \| null\)/);
  assert.match(raw, /const \[trimDragSnapS, setTrimDragSnapS\] = useState\("0"\);/);
  assert.match(raw, /const trimDragSnapInputMaxS = probe \? Math\.min\(TRIM_DRAG_SNAP_MAX_S, Math\.max\(0, Math\.floor\(probe\.durationS\)\)\) : TRIM_DRAG_SNAP_MAX_S;/);
  assert.match(raw, /const trimDragSnapIntervalS = Number\(normalizeTrimDragSnapInput\(trimDragSnapS, probe\?\.durationS \?\? null\)\);/);
  assert.match(raw, /if \(trimDragSnapIntervalS <= 0\) \{\s*updateTrimTarget\(target, rawTimeS, \{ pause: true \}\);\s*return;\s*\}/s);
  assert.match(raw, /const firstSnap = Math\.ceil\(minEnd \/ snapStepS\) \* snapStepS;/);
  assert.match(raw, /if \(candidates\.length === 0 \|\| Math\.abs\(candidates\[candidates\.length - 1\] - previewDurationS\) > 0\.0001\) \{\s*candidates\.push\(previewDurationS\);\s*\}/s);
  assert.match(raw, /type ComposeShortcutAction =/);
  assert.match(raw, /function runComposeShortcutAction\(action: ComposeShortcutAction\)/);
  assert.match(raw, /const handleComposeShortcutKeydown = useEffectEvent\(\(event: KeyboardEvent\) => \{/);
  assert.match(raw, /case "Space":/);
  assert.match(raw, /case "BracketLeft":/);
  assert.match(raw, /case "BracketRight":/);
  assert.match(raw, /case "ArrowLeft":/);
  assert.match(raw, /case "ArrowRight":/);
  assert.match(raw, /Drag snap \(s\)/);
  assert.match(raw, /0 disables snapping\./);
  assert.match(raw, /never allows a value above the current clip length\./);
  assert.match(raw, /Left\/Right nudges/);
  assert.doesNotMatch(raw, /Fine tune selected point/);
  assert.doesNotMatch(raw, /Preview selected/);
  assert.doesNotMatch(raw, /vfl-trim-fine-tune/);
  assert.doesNotMatch(css, /\.vfl-trim-fine-tune \{/);
  assert.doesNotMatch(css, /\.vfl-trim-nudge-row \{/);
});
