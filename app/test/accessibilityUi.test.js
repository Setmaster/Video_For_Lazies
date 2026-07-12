import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";
import {
  alignCropRectForEncoding,
  cropRectToPixels,
  isFullFramePixelCrop,
  pixelAspectToNormalizedRatio,
  resolveTrimSliderKey,
  updateCropRectFromPixelField,
} from "../src/lib/accessibility.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("numeric crop helpers keep source-pixel edits inside the frame", () => {
  const rect = { x: 0.1, y: 0.2, w: 0.5, h: 0.5 };
  assert.deepEqual(cropRectToPixels(rect, 1000, 500), {
    x: 100,
    y: 100,
    width: 500,
    height: 250,
  });

  assert.deepEqual(cropRectToPixels({ x: 0.999, y: 0.999, w: 0.001, h: 0.001 }, 40, 30), {
    x: 38,
    y: 28,
    width: 2,
    height: 2,
  });

  const tinyCrop = cropRectToPixels({ x: 1 / 3, y: 1 / 3, w: 2 / 3, h: 2 / 3 }, 3, 3);
  assert.deepEqual(tinyCrop, { x: 1, y: 1, width: 2, height: 2 });
  assert.equal(isFullFramePixelCrop(tinyCrop, 3, 3), false);
  assert.equal(isFullFramePixelCrop({ x: 0, y: 0, width: 3, height: 3 }, 3, 3), true);
  assert.deepEqual(alignCropRectForEncoding(tinyCrop), tinyCrop);
  assert.deepEqual(
    alignCropRectForEncoding(cropRectToPixels({ x: 0.125, y: 0, w: 0.875, h: 1 }, 4, 4)),
    { x: 1, y: 0, width: 2, height: 4 },
  );

  const moved = updateCropRectFromPixelField(rect, "x", 900, 1000, 500);
  assert.deepEqual(cropRectToPixels(moved, 1000, 500), {
    x: 500,
    y: 100,
    width: 500,
    height: 250,
  });

  const widened = updateCropRectFromPixelField(rect, "width", 1200, 1000, 500);
  assert.deepEqual(cropRectToPixels(widened, 1000, 500), {
    x: 100,
    y: 100,
    width: 900,
    height: 250,
  });

  const aspectLocked = updateCropRectFromPixelField(rect, "width", 400, 1000, 500, {
    aspectRatio: 2,
  });
  assert.deepEqual(cropRectToPixels(aspectLocked, 1000, 500), {
    x: 100,
    y: 100,
    width: 400,
    height: 200,
  });
});

test("pixel aspect presets convert correctly into normalized preview coordinates", () => {
  assert.equal(pixelAspectToNormalizedRatio(16 / 9, 1600, 900), 1);
  assert.equal(pixelAspectToNormalizedRatio(1, 1600, 900), 0.5625);
  assert.equal(pixelAspectToNormalizedRatio(16 / 9, 0, 900), null);
});

test("trim slider keys implement bounded ARIA slider behavior", () => {
  const base = { value: 5, min: 0, max: 10, fineStep: 0.1, coarseStep: 1 };
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowLeft" }), 4.9);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowDown" }), 4.9);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowRight" }), 5.1);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowUp" }), 5.1);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowRight", shiftKey: true }), 6);
  assert.equal(resolveTrimSliderKey({ ...base, key: "PageDown" }), 4);
  assert.equal(resolveTrimSliderKey({ ...base, key: "PageUp" }), 6);
  assert.equal(resolveTrimSliderKey({ ...base, key: "Home" }), 0);
  assert.equal(resolveTrimSliderKey({ ...base, key: "End" }), 10);
  assert.equal(resolveTrimSliderKey({ ...base, key: "Escape" }), null);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowLeft", value: 0 }), 0);
  assert.equal(resolveTrimSliderKey({ ...base, key: "ArrowRight", value: 10 }), 10);
});

test("crop, trim, modal, and live-status accessibility contracts are wired", async () => {
  const [app, cropFields, trimHandle, modal, cropper, css] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/components/CropPixelFields.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/components/TrimSliderHandle.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/components/ModalDialog.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/components/VideoCropper.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8"),
  ]);

  assert.match(app, /<CropPixelFields/);
  assert.match(cropFields, /<legend>Crop rectangle in source pixels<\/legend>/);
  for (const label of ["Crop X", "Crop Y", "Crop width", "Crop height"]) {
    assert.match(cropFields, new RegExp(`label: "${label}"`));
  }
  assert.match(cropFields, /type="number"/);
  assert.match(cropFields, /event\.key !== "ArrowUp" && event\.key !== "ArrowDown"/);
  assert.match(cropFields, /if \(preserveRawDraft\) nextDraft\[field\] = rawValue;/);

  assert.match(trimHandle, /role="slider"/);
  assert.match(trimHandle, /aria-orientation="horizontal"/);
  assert.match(trimHandle, /aria-valuemin=\{min\}/);
  assert.match(trimHandle, /aria-valuemax=\{max\}/);
  assert.match(trimHandle, /aria-valuenow=\{value\}/);
  assert.match(trimHandle, /aria-valuetext=\{valueText\}/);
  assert.match(css, /\.vfl-trim-timeline-grab \{[^}]*width: 24px;[^}]*height: 24px;/s);

  assert.match(modal, /createPortal/);
  assert.match(modal, /root\.inert = true;/);
  assert.match(modal, /root\.setAttribute\("aria-hidden", "true"\);/);
  assert.match(modal, /document\.addEventListener\("focusin", handleFocusIn, true\);/);
  assert.match(modal, /if \(active instanceof Node && dialog\.contains\(active\)\) return;/);
  assert.match(modal, /requestCloseRef\.current\?\.\(\);/);
  assert.match(modal, /returnFocus\?\.isConnected/);
  assert.match(app, /role="alertdialog"/);
  assert.match(app, /closeOnBackdrop/);
  assert.match(css, /\.vfl-reset-confirmation \{[^}]*background: var\(--panel\);[^}]*box-shadow: var\(--shadow-lg\);/s);
  assert.match(app, /const modalOpen = elevatedUpdateRun \|\| aboutOpen \|\| recipeDialog !== null \|\| resetConfirmationOpen;/);
  assert.match(app, /!modalOpenRef\.current/);

  assert.match(app, /id="vfl-crop-detect-status"/);
  assert.match(app, /aria-live="polite"/);
  assert.match(app, /aria-atomic="true"/);
  assert.match(app, /"accessibility-ready"/);
  assert.match(cropper, /function pointerThresholds\(\)/);
  assert.match(cropper, /displayPointToSourcePoint/);
  assert.match(cropper, /sourceRectToDisplayRect/);
  assert.match(cropper, /pixelAspectToNormalizedRatio\(aspect\.ratio, videoSize\.w, videoSize\.h\)/);
  assert.match(app, /data-smoke-id="reset-all-cancel"/);
  assert.match(app, /data-smoke-id="reset-all-confirm"/);

  const resetDialogClass = app.indexOf('className="vfl-reset-confirmation"');
  const resetDialogStart = app.lastIndexOf("<ModalDialog", resetDialogClass);
  const resetDialogEnd = app.indexOf("</ModalDialog>", resetDialogStart);
  const resetDialog = app.slice(resetDialogStart, resetDialogEnd);
  const resetCancel = resetDialog.indexOf('data-smoke-id="reset-all-cancel"');
  const resetConfirm = resetDialog.indexOf('data-smoke-id="reset-all-confirm"');
  assert.ok(resetDialogStart >= 0 && resetDialogEnd > resetDialogStart, "reset confirmation must be a modal alertdialog");
  assert.match(resetDialog, /initialFocus="first"/);
  assert.match(resetDialog, /onRequestClose=\{\(\) => setResetConfirmationOpen\(false\)\}/);
  assert.ok(resetCancel >= 0 && resetConfirm > resetCancel, "safe Cancel must receive first focus before destructive confirmation");
  assert.match(resetDialog, /data-smoke-id="reset-all-cancel"[\s\S]*?onClick=\{\(\) => setResetConfirmationOpen\(false\)\}/);
  assert.match(resetDialog, /data-smoke-id="reset-all-confirm"[\s\S]*?onClick=\{performResetAllSettings\}/);

  const resetStart = app.indexOf("function performResetAllSettings()");
  const resetEnd = app.indexOf("function applyFullRecipeSettings", resetStart);
  const resetSettings = app.slice(resetStart, resetEnd);
  assert.match(resetSettings, /cropDetectionRevisionRef\.current \+= 1;/);
  assert.match(resetSettings, /setCropDetecting\(false\);/);

  const detectStart = app.indexOf("async function autoDetectCrop()");
  const detectEnd = app.indexOf("async function startEncode", detectStart);
  const autoDetect = app.slice(detectStart, detectEnd);
  assert.match(autoDetect, /const requestRevision = \+\+cropDetectionRevisionRef\.current;/);
  assert.match(autoDetect, /cropDetectionRevisionRef\.current !== requestRevision/);
  assert.match(autoDetect, /inputPathRef\.current !== requestedPath/);
  assert.match(autoDetect, /catch \{[\s\S]*?setCropDetectHint\(CROP_DETECTION_PUBLIC_ERROR\);/);
  assert.doesNotMatch(autoDetect, /e instanceof Error|setCropDetectHint\(msg\)/);
  assert.match(autoDetect, /finally \{\s*if \(cropDetectionRevisionRef\.current === requestRevision\) \{\s*setCropDetecting\(false\);/s);

  assert.match(app, /Packaged G7 reset confirmation did not open as an alert dialog with safe Cancel focus\./);
});

test("portable Windows smoke requires mounted accessibility evidence", async () => {
  const [app, runner, windows] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/run-portable-export-smoke.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../scripts/windows-portable-export-smoke.ps1"), "utf8"),
  ]);

  assert.match(app, /runSmokeAccessibilityChecks/);
  assert.match(app, /getCurrentWebview\(\)\.setFocus\(\)/);
  assert.match(app, /focusSmokeWebviewTarget/);
  assert.match(app, /document\.hasFocus\(\) && document\.activeElement === target/);
  assert.match(app, /captureTrustedSmokeKey/);
  assert.match(app, /event\.isTrusted/);
  assert.match(app, /captureTrustedSmokeKey\(startSlider, "Tab"\)/);
  assert.match(app, /Mounted accessibility checks passed/);
  assert.match(app, /getBoundingClientRect\(\)/);
  assert.match(app, /document\.activeElement !== aboutTrigger/);
  assert.match(app, /lost modal focus when the focused update-check control became disabled/);
  assert.match(runner, /"accessibility-ready"/);
  assert.match(runner, /"workflow-ready"/);
  assert.match(runner, /!stage\.startsWith\("keyboard-"\)/);
  assert.match(windows, /"accessibility-ready"/);
  assert.match(windows, /"workflow-ready"/);
  assert.match(windows, /SendInput\(uint inputCount/);
  assert.match(windows, /KEYEVENTF_EXTENDEDKEY/);
  assert.match(windows, /SendVirtualKey\(\$handle, \[UInt16\]\$virtualKeys\[\$key\], \$isExtendedKey\)/);
  assert.doesNotMatch(windows, /System\.Windows\.Forms\.SendKeys|keybd_event/);
  assert.match(windows, /"keyboard-trim-ready"/);
  assert.match(windows, /"keyboard-trim-incremented"/);
  assert.match(windows, /"keyboard-crop-ready"/);
  assert.match(windows, /"keyboard-modal-ready"/);
  assert.match(windows, /"keyboard-complete"/);
});
