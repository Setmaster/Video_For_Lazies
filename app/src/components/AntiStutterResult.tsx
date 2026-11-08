import type { AntiStutterDiagnostics } from "../lib/types";
import { antiStutterSummary } from "../lib/interpolation";

const REASONS: Record<string, string> = {
  repaired: "Repaired",
  removed: "Repeated time removed",
  excessiveHold: "Hold too long",
  missingMotionContext: "Not enough surrounding motion",
  sceneCut: "Scene or lighting change",
  uncertainNearDuplicate: "Similar frames, uncertain freeze",
  uncertainMotion: "Possible real motion",
  unreliableCorrespondence: "Could not match surrounding motion",
  gradualDrift: "Gradual movement, not a stable hold",
  cadence: "Repeated frame cadence",
  regularCadence: "Cadence smoothed, timing preserved",
  insufficientCadence: "Pause filled, timing preserved",
  resourceLimit: "Analysis limit",
  noOutputSamples: "No output frames inside this interval",
};

export function AntiStutterResult({ value }: { value?: AntiStutterDiagnostics | null }) {
  if (!value) return null;
  return <div className="vfl-export-result-note" data-smoke-id="anti-stutter-result">
    <div>{antiStutterSummary(value)}</div>
    {value.mode === "remove" && (value.removedRuns ?? 0) > 0 ? <div className="vfl-inline-hint">Removed time is measured before speed or looping.</div> : null}
    {value.intervals.length ? <details>
      <summary>Anti-stutter intervals</summary>
      <div className="vfl-inline-hint">Times are relative to the original selected trim, before removal, speed or reverse.</div>
      <ul>
        {value.intervals.map((interval, index) => <li key={index}>
          {interval.startS.toFixed(2)}s to {interval.endS.toFixed(2)}s: {Object.prototype.hasOwnProperty.call(REASONS, interval.reason) ? REASONS[interval.reason] : "Left unchanged"}
        </li>)}
      </ul>
      {value.candidateRuns > value.intervals.length ? <div className="vfl-inline-hint">Showing the first {value.intervals.length} candidates. Counts include the whole export.</div> : null}
    </details> : null}
  </div>;
}
