import { useEffect, useMemo, useState } from "react";
import {
  cropRectToPixels,
  updateCropRectFromPixelField,
  type CropPixelField,
  type CropPixelRect,
} from "../lib/accessibility";
import type { NormalizedRect } from "./VideoCropper";

type CropPixelFieldsProps = {
  frameWidth: number;
  frameHeight: number;
  rect: NormalizedRect;
  onChange: (rect: NormalizedRect) => void;
  disabled: boolean;
  aspectRatio?: number | null;
};

const FIELDS: Array<{ field: CropPixelField; label: string }> = [
  { field: "x", label: "Crop X" },
  { field: "y", label: "Crop Y" },
  { field: "width", label: "Crop width" },
  { field: "height", label: "Crop height" },
];

function pixelDraft(rect: CropPixelRect) {
  return {
    x: String(rect.x),
    y: String(rect.y),
    width: String(rect.width),
    height: String(rect.height),
  };
}

export function CropPixelFields({
  frameWidth,
  frameHeight,
  rect,
  onChange,
  disabled,
  aspectRatio = null,
}: CropPixelFieldsProps) {
  const pixels = useMemo(
    () => cropRectToPixels(rect, frameWidth, frameHeight),
    [rect, frameWidth, frameHeight],
  );
  const [draft, setDraft] = useState(() => pixelDraft(pixels));
  const [activeField, setActiveField] = useState<CropPixelField | null>(null);

  useEffect(() => {
    if (activeField === null) {
      setDraft(pixelDraft(pixels));
    }
  }, [activeField, pixels]);

  function limits(field: CropPixelField) {
    if (field === "x") return { min: 0, max: Math.max(0, frameWidth - pixels.width) };
    if (field === "y") return { min: 0, max: Math.max(0, frameHeight - pixels.height) };
    const available = Math.max(1, field === "width" ? frameWidth - pixels.x : frameHeight - pixels.y);
    return { min: Math.min(2, available), max: available };
  }

  function updateField(field: CropPixelField, rawValue: string, preserveRawDraft = true) {
    setDraft((current) => ({ ...current, [field]: rawValue }));
    if (rawValue.trim() === "") return;

    const next = updateCropRectFromPixelField(rect, field, rawValue, frameWidth, frameHeight, {
      minCropPx: 2,
      aspectRatio,
    });
    const nextPixels = cropRectToPixels(next, frameWidth, frameHeight);
    const nextDraft = pixelDraft(nextPixels);
    if (preserveRawDraft) nextDraft[field] = rawValue;
    setDraft(nextDraft);
    onChange(next);
  }

  function finishEditing() {
    setActiveField(null);
    setDraft(pixelDraft(cropRectToPixels(rect, frameWidth, frameHeight)));
  }

  return (
    <fieldset className="vfl-crop-pixel-fields" disabled={disabled}>
      <legend>Crop rectangle in source pixels</legend>
      <div className="vfl-crop-pixel-grid">
        {FIELDS.map(({ field, label }) => {
          const { min, max } = limits(field);
          const id = `vfl-crop-${field}`;
          return (
            <div className="vfl-field" key={field}>
              <label htmlFor={id}>{label}</label>
              <input
                id={id}
                data-crop-field={field}
                type="number"
                min={min}
                max={max}
                step={1}
                value={draft[field]}
                inputMode="numeric"
                aria-describedby="vfl-crop-pixel-hint"
                onFocus={() => setActiveField(field)}
                onChange={(event) => updateField(field, event.currentTarget.value)}
                onBlur={finishEditing}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.currentTarget.blur();
                    return;
                  }
                  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
                  event.preventDefault();
                  event.stopPropagation();
                  const current = Number(event.currentTarget.value);
                  const delta = (event.shiftKey ? 10 : 1) * (event.key === "ArrowUp" ? 1 : -1);
                  updateField(field, String((Number.isFinite(current) ? current : pixels[field]) + delta), false);
                }}
              />
            </div>
          );
        })}
      </div>
      <div className="vfl-inline-hint" id="vfl-crop-pixel-hint">
        Keyboard alternative to preview dragging. Arrow Up or Down adjusts one pixel. Export rounds crop coordinates and size down to even pixels.
        {aspectRatio ? " Width and height keep the selected aspect." : ""}
      </div>
    </fieldset>
  );
}
