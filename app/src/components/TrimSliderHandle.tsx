import type { KeyboardEvent, PointerEvent } from "react";
import { resolveTrimSliderKey } from "../lib/accessibility";

type TrimSliderHandleProps = {
  id: string;
  label: string;
  value: number;
  valueText: string;
  min: number;
  max: number;
  leftPercent: number;
  active: boolean;
  disabled: boolean;
  onFocus: () => void;
  onPointerDown: (event: PointerEvent<HTMLButtonElement>) => void;
  onChange: (value: number) => void;
  onFrameStep?: (step: number) => void;
};

export function TrimSliderHandle({
  id,
  label,
  value,
  valueText,
  min,
  max,
  leftPercent,
  active,
  disabled,
  onFocus,
  onPointerDown,
  onChange,
  onFrameStep,
}: TrimSliderHandleProps) {
  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || disabled) return;
    if (onFrameStep && ["ArrowLeft", "ArrowDown", "ArrowRight", "ArrowUp", "PageDown", "PageUp"].includes(event.key)) {
      event.preventDefault(); event.stopPropagation();
      const direction = ["ArrowLeft", "ArrowDown", "PageDown"].includes(event.key) ? -1 : 1;
      onFrameStep(direction * (event.shiftKey || event.key.startsWith("Page") ? 10 : 1));
      return;
    }
    const next = resolveTrimSliderKey({
      key: event.key,
      shiftKey: event.shiftKey,
      value,
      min,
      max,
      fineStep: 0.1,
      coarseStep: 1,
    });
    if (next === null) return;

    event.preventDefault();
    event.stopPropagation();
    onChange(next);
  }

  return (
    <button
      id={id}
      type="button"
      role="slider"
      className={`vfl-trim-timeline-grab ${active ? "active" : ""}`}
      style={{ left: `${leftPercent}%` }}
      onPointerDown={onPointerDown}
      onFocus={onFocus}
      onKeyDown={handleKeyDown}
      disabled={disabled}
      aria-label={label}
      aria-orientation="horizontal"
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={value}
      aria-valuetext={valueText}
    />
  );
}
