import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";

export type InterpolationModelOption<Id extends string> = {
  id: Id;
  label: string;
  description: string;
  noncommercial?: boolean;
  disabled?: boolean;
};

export function InterpolationModelPicker<Id extends string>({ id, value, options, disabled = false, onChange }: {
  id: string;
  value: Id;
  options: readonly InterpolationModelOption<Id>[];
  disabled?: boolean;
  onChange: (value: Id) => void;
}) {
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popupRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef({ text: "", time: 0 });
  const [open, setOpen] = useState(false);
  const [activeId, setActiveId] = useState(value);
  const [position, setPosition] = useState({ top: 0, left: 0, width: 0, maxHeight: 240 });
  const selected = options.find((option) => option.id === value);
  const available = options.filter((option) => !option.disabled);
  const unavailable = disabled || available.length === 0;
  const expanded = open && !unavailable;
  const active = available.find((option) => option.id === activeId) ?? available.find((option) => option.id === value) ?? available[0];
  const described = expanded ? active : selected;
  const optionId = (option: InterpolationModelOption<Id>) => `${id}-option-${options.indexOf(option)}`;
  const descriptionId = (option: InterpolationModelOption<Id>) => `${optionId(option)}-description`;
  const description = (option: InterpolationModelOption<Id>) => <>{option.description}{option.noncommercial ? <> <span className="vfl-model-picker-noncommercial">noncommercial</span></> : null}</>;

  useEffect(() => {
    if (unavailable) setOpen(false);
  }, [unavailable]);

  useEffect(() => {
    if (!expanded) return;
    function closeOutside(event: PointerEvent) {
      if (event.target instanceof Node && !rootRef.current?.contains(event.target) && !popupRef.current?.contains(event.target)) setOpen(false);
    }
    document.addEventListener("pointerdown", closeOutside);
    return () => document.removeEventListener("pointerdown", closeOutside);
  }, [expanded]);

  useLayoutEffect(() => {
    if (!expanded) return;
    function place() {
      const bounds = triggerRef.current?.getBoundingClientRect();
      if (!bounds) return;
      const tooltipHeight = 44;
      const below = window.innerHeight - bounds.bottom - 8;
      const above = bounds.top - 8;
      const listHeight = popupRef.current?.querySelector<HTMLElement>(".vfl-model-picker-listbox")?.scrollHeight ?? 240;
      const upward = below < Math.min(240, listHeight) + tooltipHeight && above > below;
      const height = Math.min(240, Math.max(44, (upward ? above : below) - tooltipHeight));
      const popupHeight = Math.min(popupRef.current?.offsetHeight ?? height + tooltipHeight, height + tooltipHeight);
      const width = Math.min(bounds.width, window.innerWidth - 16);
      setPosition({
        top: upward ? Math.max(8, bounds.top - popupHeight - 4) : bounds.bottom + 4,
        left: Math.max(8, Math.min(bounds.left, window.innerWidth - width - 8)),
        width, maxHeight: height,
      });
    }
    place();
    const observer = new ResizeObserver(place);
    if (popupRef.current) observer.observe(popupRef.current);
    window.addEventListener("resize", place);
    document.addEventListener("scroll", place, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
      document.removeEventListener("scroll", place, true);
    };
  }, [expanded, options]);

  useEffect(() => {
    if (expanded && active) document.getElementById(optionId(active))?.scrollIntoView({ block: "nearest" });
  }, [expanded, activeId]);

  function show() {
    if (unavailable) return;
    setActiveId(available.find((option) => option.id === value)?.id ?? available[0].id);
    searchRef.current = { text: "", time: 0 };
    setOpen(true);
  }

  function choose(option: InterpolationModelOption<Id>) {
    if (unavailable || option.disabled) return;
    setOpen(false);
    triggerRef.current?.focus();
    if (option.id !== value) onChange(option.id);
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (unavailable) return;
    const { key } = event;
    if (key === "Tab") { setOpen(false); return; }
    if (key === "Escape") {
      if (expanded) { event.preventDefault(); event.stopPropagation(); setOpen(false); }
      return;
    }
    if (["Enter", " ", "ArrowDown", "ArrowUp", "Home", "End"].includes(key)) {
      event.preventDefault();
      event.stopPropagation();
      if (!expanded) {
        show();
        if (key === "Home") setActiveId(available[0].id);
        if (key === "End") setActiveId(available[available.length - 1].id);
      } else if (key === "Enter" || key === " ") {
        if (active) choose(active);
      } else {
        const index = available.findIndex((option) => option.id === active?.id);
        const next = key === "Home" ? 0 : key === "End" ? available.length - 1
          : (index + (key === "ArrowDown" ? 1 : -1) + available.length) % available.length;
        setActiveId(available[next].id);
      }
      return;
    }
    if (key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault();
      event.stopPropagation();
      const character = key.toLocaleLowerCase();
      const now = Date.now();
      const previous = now - searchRef.current.time < 700 ? searchRef.current.text : "";
      const text = previous === character ? character : previous + character;
      searchRef.current = { text, time: now };
      const index = available.findIndex((option) => option.id === (expanded ? active?.id : value));
      const ordered = [...available.slice(index + 1), ...available.slice(0, index + 1)];
      const match = ordered.find((option) => option.label.toLocaleLowerCase().startsWith(text));
      if (!expanded) setOpen(true);
      if (match) setActiveId(match.id);
    }
  }

  return <div ref={rootRef} className="vfl-field vfl-model-picker" onBlur={(event) => {
    if (!event.currentTarget.contains(event.relatedTarget) && !popupRef.current?.contains(event.relatedTarget)) setOpen(false);
  }}>
    <label id={`${id}-label`} htmlFor={id}>Motion model</label>
    <button ref={triggerRef} id={id} type="button" className="vfl-model-picker-trigger" role="combobox"
      aria-labelledby={`${id}-label ${id}-value`} aria-haspopup="listbox" aria-expanded={expanded}
      aria-controls={expanded ? `${id}-listbox` : undefined}
      aria-activedescendant={expanded && active ? optionId(active) : undefined}
      aria-describedby={described ? descriptionId(described) : undefined} disabled={unavailable}
      onClick={() => { if (expanded) setOpen(false); else show(); }} onKeyDown={onKeyDown}>
      <span id={`${id}-value`}>{selected?.label ?? value}</span><span aria-hidden="true">▾</span>
    </button>
    {options.map((option) => <span key={option.id} id={descriptionId(option)} className="vfl-model-picker-description">{description(option)}</span>)}
    {expanded ? createPortal(<div ref={popupRef} className="vfl-model-picker-popup" style={{ top: position.top, left: position.left, width: position.width }}>
      <div id={`${id}-listbox`} role="listbox" aria-labelledby={`${id}-label`} className="vfl-model-picker-listbox" style={{ maxHeight: position.maxHeight }}>
        {options.map((option) => <div key={option.id} id={optionId(option)} role="option"
          aria-selected={option.id === value} aria-disabled={option.disabled || undefined}
          aria-labelledby={`${optionId(option)}-label`} aria-describedby={descriptionId(option)}
          className={`vfl-model-picker-option${option.id === active?.id ? " active" : ""}`}
          onPointerMove={(event) => { if (event.pointerType !== "touch" && !option.disabled) setActiveId(option.id); }}
          onMouseDown={(event) => event.preventDefault()} onClick={() => choose(option)}>
          <span className="vfl-model-picker-option-name"><span id={`${optionId(option)}-label`}>{option.label}</span><span aria-hidden="true">{option.id === value ? "✓" : ""}</span></span>
          <span className="vfl-model-picker-touch-description" aria-hidden="true">{description(option)}</span>
        </div>)}
      </div>
      {active ? <div className="vfl-model-picker-tooltip" role="tooltip" aria-hidden="true">{description(active)}</div> : null}
    </div>, document.body) : null}
  </div>;
}
