import test from "node:test";
import assert from "node:assert/strict";
import {
  acceptFastTrimBounds,
  beginFastTrimCheck,
  createFastTrimState,
  fastTrimConsentForRequest,
  fastTrimConsentsMatch,
  fastTrimDurationFromRequest,
  fastTrimRequestFingerprint,
  fastTrimStateForPresentation,
  fastTrimStateIsAccepted,
  formatFastTrimDeltaUs,
  formatFastTrimTimeUs,
  invalidateFastTrimState,
  pathFreeFastTrimMessage,
  settleFastTrimCheck,
  summarizeFastTrimInspection,
  summarizeTrimResult,
} from "../src/lib/fastTrim.mjs";

function request(overrides = {}) {
  return {
    inputPath: "/private/source.mp4",
    outputPath: "/private/output.mp4",
    format: "mp4",
    title: "Replacement title",
    sizeLimitMb: 0,
    audioEnabled: true,
    normalizeAudio: false,
    stripMetadata: true,
    colorPolicy: "auto",
    advanced: {
      videoCodec: "auto",
      audioBitrateKbps: null,
      videoQuality: "auto",
      encodeSpeed: "auto",
      frameRateCapFps: null,
      audioChannels: "auto",
    },
    trim: {
      startS: 2.35,
      endS: 5.65,
      mode: "fastCopy",
      fastCopyConsent: null,
    },
    crop: null,
    reverse: false,
    speed: 1,
    rotateDeg: 0,
    resize: { mode: "source", maxEdgePx: null, widthPx: null, heightPx: null },
    maxEdgePx: null,
    color: null,
    perturbFirstFrame: false,
    loopVideo: false,
    strictFit: false,
    strictFitAllowAudioRemoval: false,
    subtitlePath: null,
    ...overrides,
  };
}

const consent = {
  planSchema: 1,
  confirmationToken: "opaque-confirmation",
  requestedStartUs: 2_350_000,
  requestedEndUs: 5_650_000,
  effectiveStartUs: 2_000_000,
  effectiveEndUs: 6_000_000,
  videoPacketCount: 120,
};

const readyInspection = {
  status: "ready",
  reasons: [],
  requestedStartUs: 2_350_000,
  requestedEndUs: 5_650_000,
  effectiveStartUs: 2_000_000,
  effectiveEndUs: 6_000_000,
  startExpansionUs: 350_000,
  endExpansionUs: 350_000,
  requiresAcceptance: true,
  videoPacketCount: 120,
  videoAction: "copy",
  audioAction: "copy",
  consent,
};

test("Fast Trim fingerprint binds compatibility and consent-token facts but excludes destination and derived values", () => {
  const base = request();
  const fingerprint = fastTrimRequestFingerprint(base);
  assert.equal(
    fastTrimRequestFingerprint(request({
      outputPath: "/another/output.mp4",
      trim: { ...base.trim, fastCopyConsent: consent },
    })),
    fingerprint,
  );
  assert.equal(fastTrimRequestFingerprint(request({ title: "  Replacement title  " })), fingerprint);
  assert.notEqual(fastTrimRequestFingerprint(request({ title: "Another title" })), fingerprint);
  assert.notEqual(fastTrimRequestFingerprint(request({ stripMetadata: false })), fingerprint);
  assert.notEqual(fastTrimRequestFingerprint(request({ audioEnabled: false })), fingerprint);
  assert.notEqual(fastTrimRequestFingerprint(request({ crop: { x: 0, y: 0, width: 640, height: 360 } })), fingerprint);
  assert.notEqual(
    fastTrimRequestFingerprint(request({ trim: { ...base.trim, startS: 2.5 } })),
    fingerprint,
  );
  assert.notEqual(fastTrimRequestFingerprint(request({ inputPath: "/private/other.mp4" })), fingerprint);
  const exact = request({ trim: { startS: 2.35, endS: 5.65, mode: "exact" } });
  assert.equal(
    fastTrimRequestFingerprint({ ...exact, trim: { startS: 2.35, endS: 5.65 } }),
    fastTrimRequestFingerprint(exact),
  );
});

test("Fast Trim state ignores stale responses and requires concrete acceptance only for expanded bounds", () => {
  const fingerprint = fastTrimRequestFingerprint(request());
  assert.ok(fingerprint);
  let state = beginFastTrimCheck(createFastTrimState(), fingerprint, 7);
  const stale = settleFastTrimCheck(state, fingerprint, 6, readyInspection);
  assert.equal(stale, state);

  state = settleFastTrimCheck(state, fingerprint, 7, readyInspection);
  assert.equal(state.phase, "ready");
  assert.equal(fastTrimStateIsAccepted(state, fingerprint), false);
  assert.equal(fastTrimConsentForRequest(state, fingerprint), null);

  state = acceptFastTrimBounds(state, true);
  assert.equal(fastTrimStateIsAccepted(state, fingerprint), true);
  assert.deepEqual(fastTrimConsentForRequest(state, fingerprint), consent);
  assert.equal(fastTrimStateForPresentation(state, fingerprint), state);

  const changedFingerprint = fastTrimRequestFingerprint(request({ title: "Changed after acceptance" }));
  const presentation = fastTrimStateForPresentation(state, changedFingerprint);
  assert.equal(presentation.phase, "stale");
  assert.equal(presentation.inspection, null);
  assert.equal(presentation.acceptedConfirmationToken, null);
  assert.match(presentation.error, /Settings changed/);

  state = invalidateFastTrimState(state);
  assert.equal(state.phase, "stale");
  assert.equal(state.acceptedConfirmationToken, null);
  assert.equal(fastTrimStateIsAccepted(state, fingerprint), false);

  const alignedInspection = {
    ...readyInspection,
    effectiveStartUs: readyInspection.requestedStartUs,
    effectiveEndUs: readyInspection.requestedEndUs,
    startExpansionUs: 0,
    endExpansionUs: 0,
    requiresAcceptance: false,
    consent: {
      ...consent,
      effectiveStartUs: consent.requestedStartUs,
      effectiveEndUs: consent.requestedEndUs,
    },
  };
  state = settleFastTrimCheck(
    beginFastTrimCheck(createFastTrimState(), fingerprint, 8),
    fingerprint,
    8,
    alignedInspection,
  );
  assert.equal(fastTrimStateIsAccepted(state, fingerprint), true);
});

test("Fast Trim consent, duration, and path-free presentation stay deterministic", () => {
  assert.equal(fastTrimConsentsMatch(consent, { ...consent }), true);
  assert.equal(fastTrimConsentsMatch(consent, { ...consent, videoPacketCount: 121 }), false);
  assert.equal(
    fastTrimDurationFromRequest(request({ trim: { ...request().trim, fastCopyConsent: consent } })),
    4,
  );
  assert.equal(formatFastTrimTimeUs(3_650_000), "0:03.650");
  assert.equal(formatFastTrimDeltaUs(350_000), "+0.350 s");
  assert.match(summarizeFastTrimInspection(readyInspection), /2\.000 to 0:06\.000/);
  assert.equal(
    summarizeTrimResult({
      mode: "fastCopy",
      requestedStartUs: 2_350_000,
      requestedEndUs: 5_650_000,
      effectiveStartUs: 2_000_000,
      effectiveEndUs: 6_000_000,
      actualStartUs: 2_000_000,
      actualEndUs: 6_000_000,
      videoPacketCount: 120,
      videoAction: "copy",
      audioAction: "copy",
      ffmpegInvocations: 1,
      commandPreview: "ffmpeg <input> <output>",
    }),
    "Fast trim, no re-encode: expected retained source 0:02.000 to 0:06.000; measured retained source 0:02.000 to 0:06.000.",
  );

  const message = pathFreeFastTrimMessage(
    "Could not inspect /private/source.mp4 or source.mp4",
    ["/private/source.mp4"],
  );
  assert.doesNotMatch(message, /private|source\.mp4/);
  assert.match(message, /selected file/);
});
