import { frameDisplaySeek } from "./frameStep.mjs";

const BUDGET_MS = 2500;
const superseded = () => Object.assign(new Error("Frame navigation was superseded."), { name: "AbortError" });

// Capture eligibility before replacing the old owner, then release its listeners.
// The new transaction still checks the source/video and stable current clock.
export function takeFrameProof(proof) {
  if (!proof) return null;
  const reusable = proof.isReusable();
  proof.dispose();
  return { ...proof, isReusable: () => reusable, dispose() {} };
}

// rVFC proves a presented picture; seeked/currentTime alone do not. An unchanged
// paused picture need not generate a new compositor callback. Reuse only its
// still-valid source-owned proof; otherwise require a real matching callback.
export function beginVerifiedFrameSeek({ video, sourceKey, timeS, endS, timelineOffsetS,
  isCurrent, previousProof = null, onPosition, onInterrupted,
  setTimer = setTimeout, clearTimer = clearTimeout }) {
  const { seekS, toleranceS } = frameDisplaySeek(timeS, endS);
  const clockToleranceS = Math.max(toleranceS, .000001);
  if (!video || video.readyState < 1 || typeof video.requestVideoFrameCallback !== "function") {
    throw new Error("This preview cannot verify individual source frames.");
  }
  const target = { video, sourceKey, logicalS: timeS, endS, clockToleranceS,
    mediaS: seekS + timelineOffsetS, presentedMediaS: timeS + timelineOffsetS };
  if (!Number.isFinite(video.duration) || !Number.isFinite(target.mediaS) || target.mediaS < 0 || target.mediaS >= video.duration) {
    throw new Error("This source frame is outside the available preview.");
  }
  const mediaSource = video.currentSrc || video.src;
  const prior = previousProof?.video === video && previousProof.sourceKey === sourceKey ? previousProof : null;
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  let settled = false, callback = null, readinessTimer = null, deadlineTimer = null;
  let latest = null, highestPresentedFrames = -1;
  const current = () => isCurrent() && (video.currentSrc || video.src) === mediaSource;
  const alive = () => !settled && current();
  const atClock = position => Math.abs(video.currentTime - position.mediaS) <= (position.clockToleranceS ?? clockToleranceS);
  const stable = position => video.paused && !video.seeking && video.readyState >= 2 && atClock(position);
  const matches = (metadata, position) => Number.isFinite(metadata.mediaTime) &&
    Math.abs(metadata.mediaTime - position.presentedMediaS) <= toleranceS;
  function cleanup() {
    if (deadlineTimer !== null) clearTimer(deadlineTimer);
    if (readinessTimer !== null) clearTimer(readinessTimer);
    if (callback !== null) video.cancelVideoFrameCallback(callback);
    for (const event of ["seeked", "timeupdate"]) video.removeEventListener(event, check);
    video.removeEventListener("seeking", interrupt);
    video.removeEventListener("play", interrupt);
    video.removeEventListener("emptied", cancel);
    video.removeEventListener("abort", cancel);
    video.removeEventListener("error", error);
  }
  function finish(failure, result) {
    if (settled) return;
    settled = true;
    cleanup();
    if (failure) { reject(failure); return; }
    // A later away-and-back seek/play can leave the same clock but invalidate
    // presentation proof. One disposable observer also catches a newer picture.
    let reusable = true, proofCallback = null;
    const events = ["seeking", "play", "emptied", "abort", "error"];
    const invalidate = () => {
      reusable = false;
      if (proofCallback !== null) video.cancelVideoFrameCallback(proofCallback);
      proofCallback = null;
    };
    const dispose = () => { invalidate(); for (const event of events) video.removeEventListener(event, invalidate); };
    const watch = (_now, metadata) => {
      proofCallback = null;
      if (!reusable) return;
      if (!current() || !stable(result) || !matches(metadata, result)) { invalidate(); return; }
      proofCallback = video.requestVideoFrameCallback(watch);
    };
    for (const event of events) video.addEventListener(event, invalidate);
    proofCallback = video.requestVideoFrameCallback(watch);
    resolve({ ...result, isReusable: () => reusable && current() && stable(result), dispose });
  }
  function cancel() { finish(superseded()); }
  function error() { finish(new Error("The preview could not display this source frame.")); }
  function interrupt() {
    if (!alive()) { cancel(); return; }
    if (!video.paused || !atClock(target)) { cancel(); onInterrupted?.(); }
  }
  function check() {
    if (!alive()) { cancel(); return; }
    if (!latest || !matches(latest, target)) return;
    if (stable(target)) {
      finish(null, { ...target, presentedMediaS: latest.mediaTime, presentedFrames: latest.presentedFrames });
    } else if (readinessTimer === null) {
      // A matching compositor callback may precede seek completion. Observe the
      // settled state even if the queued seeked event is delayed or absent.
      // This never supplies missing picture proof or extends the total budget.
      readinessTimer = setTimer(() => { readinessTimer = null; check(); }, 16);
    }
  }
  function arm() {
    callback = video.requestVideoFrameCallback((_now, metadata) => {
      if (!alive()) { cancel(); return; }
      callback = null;
      if (Number.isFinite(metadata.presentedFrames) && metadata.presentedFrames < highestPresentedFrames) { arm(); return; }
      if (Number.isFinite(metadata.presentedFrames)) highestPresentedFrames = metadata.presentedFrames;
      // A newer wrong picture invalidates an earlier match before completion.
      latest = matches(metadata, target) ? metadata : null;
      check();
      if (alive()) arm();
    });
  }
  video.pause();
  if (!current()) { cancel(); return { promise, cancel }; }
  if (prior?.logicalS === timeS && prior.endS === endS && prior.isReusable?.() &&
    matches({ mediaTime: prior.presentedMediaS }, target) && stable(prior)) {
    onPosition(prior.mediaS);
    finish(null, prior);
    return { promise, cancel };
  }
  for (const event of ["seeked", "timeupdate"]) video.addEventListener(event, check);
  video.addEventListener("seeking", interrupt);
  video.addEventListener("play", interrupt);
  video.addEventListener("emptied", cancel);
  video.addEventListener("abort", cancel);
  video.addEventListener("error", error);
  deadlineTimer = setTimer(() => {
    if (!alive()) { cancel(); return; }
    check();
    if (!settled) finish(new Error("The preview could not verify the requested source frame."));
  }, BUDGET_MS);
  onPosition(target.mediaS);
  arm(); // Register before assigning currentTime, including synchronous events.
  try { video.currentTime = target.mediaS; check(); } catch { error(); }
  return { promise, cancel };
}
