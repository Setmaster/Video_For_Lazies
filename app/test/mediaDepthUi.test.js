import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("media-depth policy is wired through requests, resets, capability gating, and plan UI", async () => {
  const [app, types, css] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/types.ts"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8"),
  ]);

  assert.match(types, /export type ColorPolicy = "auto" \| "standardSdr";/);
  assert.match(types, /dynamicRange\?: DynamicRange/);
  assert.match(types, /sampleAspectRatio\?: Rational/);
  assert.match(types, /unsupportedRotationDeg\?: number \| null/);
  assert.match(types, /features\?: EncodeFeatureCapability/);
  assert.match(app, /const \[colorPolicy, setColorPolicy\] = useState<ColorPolicy>\("auto"\);/);
  assert.match(app, /colorPolicy: "auto",/);
  assert.match(app, /colorPolicy,/);
  assert.ok((app.match(/setColorPolicy\("auto"\)/g) ?? []).length >= 3, "new input and resets must clear source consent");
  assert.match(app, /available: false,/);
  assert.doesNotMatch(app, /describeMissingFeature\("coreExport", "Core export support"/);
  assert.match(app, /describeMissingFeature\("hdrToSdr"/);
  assert.match(app, /probe\?\.dynamicRange === "hdr10"[\s\S]*?\["format", "tonemap", "zscale"\][\s\S]*?\["format", "zscale"\]/);
  assert.match(app, /describeMissingFeature\("sarNormalize"/);
  assert.match(app, /describeMissingFeature\("reverseLoop"/);
  assert.match(app, /transformRequiredFilters/);
  assert.match(app, /const trimIsActive =/);
  assert.match(app, /trimRequestIsActive\(\{/);
  assert.match(app, /const cropFilterPlanned = Boolean/);
  assert.match(app, /const evenOutputFilterPlanned = cropFilterPlanned/);
  assert.doesNotMatch(app, /const hasVideoEditTransforms =\s*\n\s*Boolean\(trimSummary\)/);
  assert.match(app, /const hasVideoEditTransforms =[\s\S]*?perturbFirstFrame[\s\S]*?;/);
  assert.match(app, /currentPlanRequiresVideoEncoder/);
  assert.match(app, /const sizeTargetRetainedAudioNeedsEncode = Boolean\(plannedSummary\?\.audioIncluded\)/);
  assert.match(app, /sizeTargetRequiresVideoEncoder/);
  assert.match(app, /!sourceVideoCopyCompatible/);
  assert.match(app, /plannedSummary\?\.audioIncluded/);
  assert.match(app, /codec\.format === format && codec\.available && codec\.isDefault/);
  assert.match(app, /sizeTargetRetainsAudio\(\{/);
  const audioEncoderGate = app.match(/const currentPlanRequiresAudioEncoder =([\s\S]*?);\r?\n/);
  assert.ok(audioEncoderGate, "request-specific audio encoder gate must exist");
  assert.doesNotMatch(audioEncoderGate[1], /sizeLimitEnabled/);
  assert.match(audioEncoderGate[1], /!sourceAudioCopyCompatible/);
  assert.match(app, /sourceRotationBlockingReason/);
  assert.match(app, /sourceDimensionBlockingReason/);
  assert.match(app, /Video export requires a source at least 2x2 pixels/);
  assert.match(app, /Crop width and height must each be at least 2 pixels/);
  assert.match(app, /frameExportBlockingReason/);
  assert.match(app, /Save Frame unavailable/);
  assert.match(app, /Convert \{colorSource\.label\} to standard SDR for sharing/);
  assert.match(app, /requires Auto or H\.264 \(libx264\)/);
  assert.match(app, /Custom dimensions are authoritative and may change the visible shape/);
  assert.match(app, /Ambiguous PQ\/HDR metadata/);
  assert.match(app, /setReverse\(smokeConfig\.reverse \?\? false\)/);
  assert.match(app, /Transform memory/);
  assert.match(app, /Source media facts/);
  assert.match(css, /\.vfl-memory-status\.blocked/);
  assert.match(css, /\.vfl-color-policy/);
});
