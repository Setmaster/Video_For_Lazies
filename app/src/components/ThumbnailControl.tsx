import { useRef } from "react";
import type { ThumbnailInspection, ThumbnailSettings } from "../lib/types";
import "./ThumbnailControl.css";

export function ThumbnailControl({ value, inspection, inspecting, error, disabled, unavailable, outputAspect, onChoose, onChange, onRemove }: {
  value: ThumbnailSettings | null;
  inspection: ThumbnailInspection | null;
  inspecting: boolean;
  error: string | null;
  disabled: boolean;
  unavailable: string | null;
  outputAspect: number;
  onChoose: () => void;
  onChange: (value: ThumbnailSettings) => void;
  onRemove: () => void;
}) {
  const name = value?.imagePath.split(/[\\/]/).pop();
  const chooseRef = useRef<HTMLButtonElement>(null);
  return <div className="vfl-stack-md">
    <p className="vfl-inline-hint" id="vfl-thumbnail-help">Replace the first video frame with an image, without adding time. Websites may choose a different thumbnail.</p>
    {value && inspection && <div className="vfl-thumbnail-preview" style={{ aspectRatio: outputAspect, maxWidth: `${240 * outputAspect}px` }}>
      <img src={inspection.previewDataUrl} alt={`First-frame thumbnail: ${name}`} style={{ objectFit: value.fit === "fill" ? "cover" : "contain" }} />
    </div>}
    {value && <div className="vfl-field">
      <label htmlFor="vfl-thumbnail-fit">Image fit</label>
      <select id="vfl-thumbnail-fit" value={value.fit} disabled={disabled || inspecting} onChange={event => onChange({ ...value, fit: event.currentTarget.value as "fit" | "fill" })}>
        <option value="fit">Fit (show the whole image)</option>
        <option value="fill">Fill (crop the edges)</option>
      </select>
      <div className="vfl-inline-hint">Fit adds black bars when needed. Transparent areas appear black. Animated images use their first frame.</div>
    </div>}
    <div className="vfl-thumbnail-actions">
      <button ref={chooseRef} type="button" data-smoke-id="thumbnail-choose" onClick={onChoose} disabled={disabled || inspecting || Boolean(unavailable)} aria-busy={inspecting} aria-describedby="vfl-thumbnail-help">{inspecting ? "Validating…" : value ? "Replace image…" : "Choose image…"}</button>
      {value && <button type="button" data-smoke-id="thumbnail-remove" onClick={() => { onRemove(); window.requestAnimationFrame(() => chooseRef.current?.focus()); }} disabled={disabled || inspecting}>Remove</button>}
    </div>
    <div className="vfl-inline-hint" role="status" aria-live="polite">{inspecting ? "Validating thumbnail image…" : name ?? "No thumbnail image selected."}</div>
    {unavailable && <div className="vfl-inline-hint">{unavailable}</div>}
    {error && <div className="vfl-error" role="alert">{error}</div>}
  </div>;
}
