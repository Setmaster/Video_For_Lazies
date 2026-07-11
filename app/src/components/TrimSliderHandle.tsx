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
}: TrimSliderHandleProps) {
  function handleKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
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
