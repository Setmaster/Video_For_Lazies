import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ts from "typescript";

// Render the production component without adding a DOM or a second renderer.
const source = readFileSync(new URL("../src/components/RecordingPreview.tsx", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
}).outputText;
const exports = {};
runInNewContext(compiled, { exports, require: createRequire(import.meta.url) });
const RecordingPreview = exports.default;

function render({ live = false, width = 0, height = 0, recordingPhase = "idle" } = {}) {
  return renderToStaticMarkup(createElement(RecordingPreview, {
    preview: {
      snapshot: { phase: live ? "previewing" : "starting", previewId: "preview-one", configEpoch: 1,
        sourceWidth: width, sourceHeight: height },
      image: live ? { previewId: "preview-one", configEpoch: 1 } : null,
      starting: !live, stopping: false, error: null, eventsReady: true, hasRequest: true,
    },
    enabled: true, canPreview: true, recording: false, onEnabledChange() {}, recordingPhase,
  }));
}

test("source dimensions reserve their layout slot before first presentation and while hidden", () => {
  for (const options of [{}, { width: 7680, height: 2160 },
    { live: true, width: 7680, height: 2160 },
    { live: true, width: 7680, height: 2160, recordingPhase: "paused" }]) {
    const html = render(options);
    const dimensions = html.match(/<div[^>]*class="[^"]*vfl-record-preview-dimensions"[^>]*>[^<]*<\/div>/g);
    assert.equal(dimensions?.length, 1, "first frame and pause must not insert/remove a layout row");
    const shown = options.live && options.recordingPhase !== "paused";
    assert.match(dimensions[0], shown ? /aria-hidden="false" style="visibility:visible"/ : /aria-hidden="true" style="visibility:hidden"/);
    if (options.width) assert.match(dimensions[0], /7680 × 2160 source pixels/);
    else assert.match(dimensions[0], />\u00a0<\/div>/, "unknown dimensions still reserve a text line");
  }
});

test("preview statuses retain their complete accessible text and tooltip when the heading narrows", () => {
  for (const options of [{}, { live: true, width: 7680, height: 2160 },
    { live: true, recordingPhase: "paused" }, { live: true, recordingPhase: "resuming" }]) {
    const html = render(options);
    const status = html.match(/<div id="vfl-record-preview-status"[^>]*role="status"[^>]*aria-live="polite"[^>]*title="([^"]*)">([^<]*)<\/div>/);
    assert.ok(status, "one status remains available to assistive technology");
    assert.equal(status[1], status[2], "the tooltip exposes the full visually truncated status");
    assert.ok(status[2].length > 0);
  }
});
