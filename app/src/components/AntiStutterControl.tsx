import type { InterpolationSettings, OutputFormat } from "../lib/types";
import { antiStutterConflict } from "../lib/interpolation";

/** A separate feature which shares Smooth motion's selected frame generator. */
export function AntiStutterControl({ value, format, disabled, onChange }: {
  value: InterpolationSettings | null;
  format: OutputFormat;
  disabled: boolean;
  onChange: (value: InterpolationSettings) => void;
}) {
  const enabled = value?.antiStutter === true;
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
      {enabled && !antiStutterConflict(value) ? <div className="vfl-inline-hint">
        May change intentional pauses, animation timing or small moving overlays. Review an exact sample with motion before and after the pause. Long holds and uncertain motion stay unchanged.
      </div> : null}
      {enabled ? <div className="vfl-inline-hint">Turning Smooth motion off also turns Anti-stutter off.</div> : null}
    </section>
  );
}
