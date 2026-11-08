import type { CSSProperties } from "react";
import type { InterpolationSettings, OutputFormat } from "../lib/types";
import { ANTI_STUTTER_DEFAULT_STRENGTH, antiStutterConflict } from "../lib/interpolation";

/** A separate feature which shares Smooth motion's selected frame generator. */
export function AntiStutterControl({ value, format, disabled, onChange }: {
  value: InterpolationSettings | null;
  format: OutputFormat;
  disabled: boolean;
  onChange: (value: InterpolationSettings) => void;
}) {
  const enabled = value?.antiStutter === true;
  const strength = value?.antiStutterStrength ?? ANTI_STUTTER_DEFAULT_STRENGTH;
  const unavailable = !value
    ? "Enable Smooth motion above to generate replacement frames."
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
            if (value) onChange({ ...value, antiStutter: event.currentTarget.checked });
          }} />
        <span>Enable Anti-stutter</span>
      </label>
      <div className="vfl-inline-hint" id="vfl-anti-stutter-description">
        {unavailable ?? "Attempts to smooth short held-frame pauses, including compressed gameplay. Keeps duration and audio in sync."}
      </div>
      {enabled && !antiStutterConflict(value) ? <>
        <div className="vfl-field vfl-slider-field">
          <div className="vfl-slider-top">
            <label htmlFor="vfl-anti-stutter-strength">Repair strength</label>
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
          Allows repair of less certain holds at higher strength. May change intentional pauses, animation or small moving details. Check an exact sample first.
        </div>
      </> : null}
      {enabled ? <div className="vfl-inline-hint">Turning Smooth motion off also turns Anti-stutter off.</div> : null}
    </section>
  );
}
