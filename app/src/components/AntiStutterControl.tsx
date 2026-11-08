import type { CSSProperties } from "react";
import type { InterpolationSettings, OutputFormat } from "../lib/types";
import { ANTI_STUTTER_DEFAULT_STRENGTH, antiStutterConflict, antiStutterMode, setAntiStutterEnabled } from "../lib/interpolation";

/** A separate feature which shares Smooth motion's selected frame generator. */
export function AntiStutterControl({ value, format, disabled, onChange }: {
  value: InterpolationSettings | null;
  format: OutputFormat;
  disabled: boolean;
  onChange: (value: InterpolationSettings) => void;
}) {
  const enabled = value?.antiStutter === true;
  const strength = value?.antiStutterStrength ?? ANTI_STUTTER_DEFAULT_STRENGTH;
  const mode = antiStutterMode(value);
  const unavailable = !value
    ? "Enable Smooth motion above to use Anti-stutter."
    : format === "mp3"
      ? "Available for MP4 and WebM video exports."
      : value.modelId === "ldf-vfi"
        ? "Choose RIFE, GMFSS or GIMM above. LDF does not support Anti-stutter."
        : null;
  return (
    <section className="vfl-anti-stutter" aria-labelledby="vfl-anti-stutter-title">
      <label className="vfl-check vfl-check-card" id="vfl-anti-stutter-title">
        <input id="vfl-anti-stutter" type="checkbox" checked={enabled}
          aria-describedby="vfl-anti-stutter-description"
          disabled={disabled || (!enabled && unavailable !== null)}
          onChange={(event) => {
            if (value) onChange(setAntiStutterEnabled(value, event.currentTarget.checked));
          }} />
        <span>Enable Anti-stutter</span>
      </label>
      <div className="vfl-inline-hint" id="vfl-anti-stutter-description">
        {unavailable ?? "Detects short repeated-frame holds, including compressed gameplay. Choose whether to remove their time or fill the pause."}
      </div>
      {enabled && !antiStutterConflict(value) ? <>
        <div className="vfl-field">
          <label htmlFor="vfl-anti-stutter-mode">Mode</label>
          <select id="vfl-anti-stutter-mode" value={mode} disabled={disabled || unavailable !== null}
            aria-describedby="vfl-anti-stutter-mode-help"
            onChange={(event) => {
              if (value) onChange({ ...value, antiStutterMode: event.currentTarget.value as "remove" | "repair" });
            }}>
            <option value="remove">Remove repeated time</option>
            <option value="repair">Fill pauses</option>
          </select>
        </div>
        <div className="vfl-inline-hint" id="vfl-anti-stutter-mode-help">
          {mode === "remove"
            ? "Shortens qualifying holds before Smooth motion; included audio is cut at the same times. Duration is calculated after cleanup."
            : "Generates motion across suspected pauses while preserving duration and audio timing."}
        </div>
        <div className="vfl-field vfl-slider-field">
          <div className="vfl-slider-top">
            <label htmlFor="vfl-anti-stutter-strength">Detection strength</label>
            <output className="vfl-slider-value" htmlFor="vfl-anti-stutter-strength">{strength}</output>
          </div>
          <input id="vfl-anti-stutter-strength" type="range" min={0} max={100} step={1}
            value={strength} disabled={disabled || unavailable !== null}
            style={{ "--fill": `${strength}%` } as CSSProperties}
            aria-describedby="vfl-anti-stutter-strength-help"
            aria-valuetext={`${strength} of 100${strength === 0 ? ", Conservative" : strength === 100 ? ", Aggressive" : ""}`}
            onChange={(event) => {
              if (value) onChange({ ...value, antiStutterStrength: Number(event.currentTarget.value) });
            }} />
          <div className="vfl-slider-top vfl-inline-hint" aria-hidden="true">
            <span>Conservative</span><span>Aggressive</span>
          </div>
        </div>
        <div className="vfl-inline-hint" id="vfl-anti-stutter-strength-help">
          Higher strength admits less certain holds. May change intentional pauses, animation or small moving details. Check an exact sample first.
        </div>
      </> : null}
      {enabled ? <div className="vfl-inline-hint">Turning Smooth motion off also turns Anti-stutter off.</div> : null}
    </section>
  );
}
