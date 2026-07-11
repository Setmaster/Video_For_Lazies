import type { FastTrimInspection, TrimMode } from "../lib/types";
import type { FastTrimUiState } from "../lib/fastTrim";
import {
  formatFastTrimDeltaUs,
  formatFastTrimTimeUs,
  summarizeFastTrimInspection,
} from "../lib/fastTrim";

type TrimModeControlsProps = {
  mode: TrimMode;
  trimActive: boolean;
  disabled: boolean;
  state: FastTrimUiState;
  accepted: boolean;
  onModeChange: (mode: TrimMode) => void;
  onCheck: () => void;
  onAcceptedChange: (accepted: boolean) => void;
};

function effectiveExpansion(
  inspection: FastTrimInspection,
  edge: "start" | "end",
) {
  if (edge === "start") {
    return inspection.startExpansionUs ?? (
      inspection.effectiveStartUs == null
        ? null
        : inspection.requestedStartUs - inspection.effectiveStartUs
    );
  }
  return inspection.endExpansionUs ?? (
    inspection.effectiveEndUs == null
      ? null
      : inspection.effectiveEndUs - inspection.requestedEndUs
  );
}

export function TrimModeControls({
  mode,
  trimActive,
  disabled,
  state,
  accepted,
  onModeChange,
  onCheck,
  onAcceptedChange,
}: TrimModeControlsProps) {
  const inspection = state.inspection;
  const readyInspection = state.phase === "ready" && inspection?.status === "ready" ? inspection : null;
  const statusText = mode === "exact"
    ? "Exact trim is selected. Requested boundaries will use decoded frame and sample trimming."
    : state.phase === "checking"
      ? "Checking Fast Trim compatibility and expected closed-GOP boundaries."
      : state.phase === "ready"
        ? summarizeFastTrimInspection(inspection)
        : state.phase === "blocked"
          ? "Fast Trim is blocked by the current source or settings."
          : state.phase === "stale"
            ? "Fast Trim needs to be checked again because the source or settings changed."
            : state.phase === "error"
              ? "Fast Trim inspection failed."
              : trimActive
                ? "Fast Trim is selected but has not been checked."
                : "Set a trim range before checking Fast Trim.";
  const alertText = mode !== "fastCopy"
    ? null
    : state.phase === "stale" || state.phase === "error"
      ? state.error
      : null;
  const checkLabel = state.phase === "idle" ? "Check Fast Trim" : "Re-check Fast Trim";

  return (
    <fieldset className="vfl-fast-trim" aria-describedby="vfl-fast-trim-warning vfl-fast-trim-live">
      <legend>Trim method</legend>
      <label className={`vfl-fast-trim-option ${mode === "exact" ? "selected" : ""}`}>
        <input
          type="radio"
          name="trim-mode"
          value="exact"
          checked={mode === "exact"}
          onChange={() => onModeChange("exact")}
          disabled={disabled}
          data-smoke-id="trim-mode-exact"
        />
        <span>
          <strong>Exact trim</strong>
          <small>Default. Uses decoded frames and samples to honor the requested boundaries as closely as the media permits.</small>
        </span>
      </label>
      <label className={`vfl-fast-trim-option ${mode === "fastCopy" ? "selected" : ""}`}>
        <input
          type="radio"
          name="trim-mode"
          value="fastCopy"
          checked={mode === "fastCopy"}
          onChange={() => onModeChange("fastCopy")}
          disabled={disabled}
          data-smoke-id="trim-mode-fast"
        />
        <span>
          <strong>Fast trim, no re-encode</strong>
          <small>Copies retained compatible streams without changing their encoded payloads; explicitly removed audio stays removed.</small>
        </span>
      </label>
      <p id="vfl-fast-trim-warning" className="vfl-fast-trim-warning">
        Fast Trim keeps a containing closed-GOP interval. It may retain material before the requested start and after the requested end.
        It never falls back to re-encoding.
      </p>

      {mode === "fastCopy" ? (
        <div className="vfl-fast-trim-check">
          <button
            type="button"
            onClick={onCheck}
            disabled={disabled || !trimActive || state.phase === "checking"}
            data-smoke-id="fast-trim-check"
          >
            {state.phase === "checking" ? "Checking Fast Trim…" : checkLabel}
          </button>
          {!trimActive ? <span className="vfl-inline-hint">Set a start later than 0 or an end before the source ends first.</span> : null}
        </div>
      ) : null}

      <div
        id="vfl-fast-trim-live"
        className="vfl-fast-trim-live"
        role="status"
        aria-live="polite"
        aria-atomic="true"
        data-smoke-id="fast-trim-live"
      >
        {statusText}
      </div>

      {mode === "fastCopy" && inspection?.status === "blocked" ? (
        <div className="vfl-fast-trim-alert" role="alert" aria-atomic="true" data-smoke-id="fast-trim-alert">
          <strong>Fast Trim cannot run with this plan.</strong>
          <ul>
            {inspection.reasons.map((reason, index) => (
              <li key={`${reason.code}-${index}`}>{reason.message}</li>
            ))}
          </ul>
          <div>Choose Exact trim or change the listed setting, then re-check.</div>
        </div>
      ) : null}
      {mode === "fastCopy" && alertText ? (
        <div className="vfl-fast-trim-alert" role="alert" aria-atomic="true" data-smoke-id="fast-trim-alert">
          {alertText}
        </div>
      ) : null}

      {mode === "fastCopy" && readyInspection ? (
        <div className="vfl-fast-trim-evidence" data-smoke-id="fast-trim-evidence">
          <table>
            <caption>Fast Trim expected source boundaries</caption>
            <thead>
              <tr>
                <th scope="col">Boundary</th>
                <th scope="col">Requested</th>
                <th scope="col">Expected retained</th>
                <th scope="col">Extra material</th>
              </tr>
            </thead>
            <tbody>
              <tr>
                <th scope="row">Start</th>
                <td>{formatFastTrimTimeUs(readyInspection.requestedStartUs)}</td>
                <td>{formatFastTrimTimeUs(readyInspection.effectiveStartUs)}</td>
                <td>{formatFastTrimDeltaUs(effectiveExpansion(readyInspection, "start"))} before</td>
              </tr>
              <tr>
                <th scope="row">End</th>
                <td>{formatFastTrimTimeUs(readyInspection.requestedEndUs)}</td>
                <td>{formatFastTrimTimeUs(readyInspection.effectiveEndUs)}</td>
                <td>{formatFastTrimDeltaUs(effectiveExpansion(readyInspection, "end"))} after</td>
              </tr>
            </tbody>
          </table>
          <div className="vfl-fast-trim-streams">
            Video {readyInspection.videoAction ?? "pending"}; audio {readyInspection.audioAction ?? "pending"}; {readyInspection.videoPacketCount ?? 0} video packets planned.
          </div>
          {readyInspection.requiresAcceptance ? (
            <>
              {!accepted ? (
                <div className="vfl-fast-trim-alert" role="alert" aria-atomic="true">
                  Review and accept the expanded boundaries before Export or Add Current Plan.
                </div>
              ) : null}
              <label className="vfl-fast-trim-accept">
                <input
                  type="checkbox"
                  checked={accepted}
                  onChange={(event) => onAcceptedChange(event.currentTarget.checked)}
                  disabled={disabled}
                  data-smoke-id="fast-trim-accept"
                />
                <span>
                  I accept the expected retained interval from {formatFastTrimTimeUs(readyInspection.effectiveStartUs)} to {formatFastTrimTimeUs(readyInspection.effectiveEndUs)},
                  including material outside my requested boundaries.
                </span>
              </label>
            </>
          ) : (
            <div className="vfl-inline-hint" data-smoke-id="fast-trim-aligned">
              The requested boundaries are already aligned. No second acknowledgment is needed.
            </div>
          )}
        </div>
      ) : null}
    </fieldset>
  );
}
