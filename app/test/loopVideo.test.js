import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const read = (rel) => fs.readFile(path.resolve(__dirname, rel), "utf8");

function sourceSection(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start);
  assert.notEqual(start, -1, `Missing source marker: ${startMarker}`);
  assert.notEqual(end, -1, `Missing source marker: ${endMarker}`);
  return source.slice(start, end);
}

test("backend wraps the filter chains as a forward+reverse boomerang", async () => {
  const video = await read("../src-tauri/src/video.rs");

  assert.match(video, /pub loop_video: bool/);
  // Video and audio are boomeranged symmetrically (split/reverse/concat) so they
  // stay in sync without a filter_complex.
  assert.match(video, /fn apply_boomerang_video/);
  assert.match(video, /split\[fv\]\[rv0\];\[rv0\]reverse\[rv\];\[fv\]\[rv\]concat=n=2:v=1/);
  assert.match(video, /fn apply_boomerang_audio/);
  assert.match(video, /asplit\[fa\]\[ra0\];\[ra0\]areverse\[ra\];\[fa\]\[ra\]concat=n=2:v=0:a=1/);
  // Loop is a no-op for audio-only mp3.
  assert.match(video, /!req\.loop_video \|\| matches!\(req\.format, OutputFormat::Mp3\)/);
  // Output duration doubles; the planner treats Loop as a timeline encode.
  assert.match(video, /if loop_video \{ base \* 2\.0 \} else \{ base \}/);
  assert.match(video, /fn timeline_requires_encode[\s\S]*\|\| request\.loop_video/);
});

test("frontend exposes a Loop toggle wired through the request", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /const \[loopVideo, setLoopVideo\] = useState\(false\);/);
  // buildRequest forwards it (forced off for audio-only mp3).
  assert.match(app, /loopVideo: format === "mp3" \? false : loopVideo,/);
  // Reset and a user-facing control exist.
  assert.match(app, /setLoopVideo\(false\);/);
  assert.match(app, /Loop \(boomerang\)/);
  // Smoke can drive it.
  assert.match(app, /setLoopVideo\(smokeConfig\.loopVideo \?\? false\);/);

  const types = await read("../src/lib/types.ts");
  assert.match(types, /loopVideo: boolean;/);
  assert.match(types, /loopVideo\?: boolean \| null;/); // smoke config
});

test("frontend Loop planning mirrors backend duration semantics", async () => {
  const app = await read("../src/App.tsx");
  const plannedSummary = sourceSection(app, "  const plannedSummary = useMemo", "  const previewDurationS =");

  // Only video exports double their planned duration, and the size-target
  // bitrate calculation consumes that adjusted duration.
  assert.match(plannedSummary, /const baseDurationS = Math\.max\(0\.001, \(endS - startS\) \/ speedNum\);/);
  assert.match(plannedSummary, /const durationS = format !== "mp3" && loopVideo \? baseDurationS \* 2 : baseDurationS;/);
  assert.match(plannedSummary, /\/ durationS \/ 1000/);
  assert.match(plannedSummary, /\r?\n    loopVideo,\r?\n/);

  const activeEditChips = sourceSection(app, "  const activeEditChips = useMemo", "  const lastExportSizeText =");
  assert.match(activeEditChips, /if \(format !== "mp3" && loopVideo\) chips\.push\("Loop"\);/);
  assert.match(activeEditChips, /\r?\n    loopVideo,\r?\n/);

  // Sample-size scaling must use the looped sample duration too, otherwise a
  // looped sample receives only half of its proportional size budget.
  assert.match(app, /const outputSampleDuration = Math\.max\(0\.1, \(sampleEnd - sampleStart\) \/ request\.speed\) \* \(request\.loopVideo \? 2 : 1\);/);
});
