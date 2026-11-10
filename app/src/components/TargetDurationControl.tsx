import { targetDurationSummary, type TargetDurationPlan } from "../lib/targetDuration";
import type { ExportDiagnostics } from "../lib/types";

export function TargetDurationControl({ enabled, value, plan, disabled, onEnabled, onChange }: {
  enabled: boolean; value: string; plan: TargetDurationPlan; disabled: boolean;
  onEnabled: (enabled: boolean) => void; onChange: (value: string) => void;
}) {
  return <div className="vfl-stack-sm">
    <label className="vfl-check">
      <input id="vfl-target-duration-enabled" type="checkbox" checked={enabled} disabled={disabled}
        onChange={event => onEnabled(event.currentTarget.checked)} />
      Target duration
    </label>
    {enabled ? <>
      <div className="vfl-field">
        <label htmlFor="vfl-target-duration">Desired final length (s)</label>
        <input id="vfl-target-duration" type="number" min={0} step="any" inputMode="decimal"
          value={value} disabled={disabled} aria-invalid={Boolean(plan.error)} aria-describedby="vfl-target-duration-help"
          onChange={event => onChange(event.currentTarget.value)} />
      </div>
      <div id="vfl-target-duration-help" className={plan.error ? "vfl-error" : "vfl-inline-hint"}
        role={plan.error ? "alert" : "status"} aria-live="polite">
        {targetDurationSummary(plan)}
      </div>
      <div className="vfl-inline-hint">Includes trim and boomerang. Turn off to restore manual speed.</div>
    </> : null}
  </div>;
}

export function TargetDurationResult({ value }: { value?: ExportDiagnostics["targetDuration"] }) {
  if (!value) return null;
  return <div className="vfl-export-result-note" data-smoke-id="target-duration-result">
    Requested {value.requestedDurationS.toFixed(3)} s; used {value.effectiveSpeed.toFixed(6).replace(/\.?0+$/, "")}× speed.
    {value.speedLimited ? " Closest supported speed reached (0.05–16×)." : value.frameRounded ? " Length follows output-frame timing." : ""}
  </div>;
}
