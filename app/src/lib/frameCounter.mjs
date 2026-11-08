// Rank actual source presentation timestamps, never an average-FPS estimate.
export function sourceFrameNumber(times, timeS, durationS, presented = false) {
  if (!times.length || !Number.isFinite(timeS) || !Number.isFinite(durationS) || durationS <= 0) return null;
  let lo = 0;
  let hi = times.length;
  while (lo < hi) {
    const mid = lo + Math.floor((hi - lo) / 2);
    if (times[mid] <= timeS && times[mid] < durationS) lo = mid + 1;
    else hi = mid;
  }
  // Browser frame-callback timestamps can be rounded to microseconds. Only
  // compensate for that when matching a presented timestamp, not an arbitrary
  // scrub time, and never bridge closely spaced source pictures.
  if (presented && lo < times.length && times[lo] < durationS) {
    const gap = lo ? times[lo] - times[lo - 1] : Infinity;
    if (times[lo] - timeS <= Math.min(0.000001, gap / 4)) lo++;
  }
  return lo || null;
}

export function validFrameIndex(result, identity) {
  return result?.sourceIdentity === identity && Array.isArray(result.timesS) &&
    result.timesS.length > 0 && result.timesS.length <= 100_000 &&
    result.timesS.every((t, i, times) => Number.isFinite(t) && (!i || t > times[i - 1]));
}
