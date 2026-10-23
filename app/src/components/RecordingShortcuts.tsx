import { useEffect, useRef, useState } from "react";
import type { RecordingControls } from "../lib/recordingControls";
import type { ShortcutAction, ShortcutChord } from "../lib/recordingControlsTypes";
import { captureShortcutEvent, SHORTCUT_ACTIONS, shortcutLabel } from "../lib/recordingControlsState";
const labels: Record<ShortcutAction, string> = { start: "Start", pause: "Pause", resume: "Resume", stop: "Stop" };
export default function RecordingShortcuts({ controls, idle, visible }: { controls: RecordingControls; idle: boolean; visible: boolean }) {
  const [open, setOpen] = useState(false), [editing, setEditing] = useState<ShortcutAction | null>(null);
  const [capturing, setCapturing] = useState(false), [pending, setPending] = useState(false), [normalizing, setNormalizing] = useState(false);
  const [candidate, setCandidate] = useState<ShortcutChord | null>(null), [heldCount, setHeldCount] = useState(0);
  const input = useRef<HTMLInputElement>(null), held = useRef(new Set<string>()), captureEpoch = useRef(0), normalization = useRef(0);
  const state = controls.snapshot;
  const closeCapture = () => { captureEpoch.current += 1; input.current?.blur(); setCapturing(false); held.current.clear(); setHeldCount(0); };
  async function begin(action: ShortcutAction) {
    if (!idle || pending || !controls.ready) return;
    setPending(true); setCandidate(null);
    try {
      if (!await controls.beginAssignment()) return;
      setEditing(action); setCapturing(true); captureEpoch.current += 1;
      requestAnimationFrame(() => input.current?.focus());
    } finally { setPending(false); }
  }
  async function finish(save: boolean) {
    if (pending) return;
    const action = editing, chord = candidate;
    closeCapture(); setPending(true);
    try {
      const bindings = save && action && chord && state ? { ...state.bindings, [action]: chord } : undefined;
      if (await controls.finishAssignment(bindings)) { setEditing(null); setCandidate(null); void controls.reconcile(); }
    } finally { setPending(false); }
  }
  async function clear(action: ShortcutAction) {
    if (!state || !idle || pending) return;
    setPending(true); closeCapture();
    try {
      if (!await controls.beginAssignment()) return;
      if (await controls.finishAssignment({ ...state.bindings, [action]: null })) { setEditing(null); void controls.reconcile(); }
    } finally { setPending(false); }
  }
  useEffect(() => {
    if (visible || !editing) return;
    captureEpoch.current += 1; input.current?.blur(); setCapturing(false);
    void controls.finishAssignment().then(ended => { if (ended) setEditing(null); });
  }, [visible, editing, controls.finishAssignment]);
  useEffect(() => () => { captureEpoch.current += 1; input.current?.blur(); }, []);
  return <section className={`vfl-card ${open ? "is-open" : ""}`} aria-labelledby="vfl-record-shortcuts-title">
    <button type="button" className="vfl-record-card-head vfl-record-shortcuts-toggle" aria-expanded={open} onClick={() => setOpen(value => !value)}>
      <span id="vfl-record-shortcuts-title" className="vfl-card-title">Shortcuts</span><span className="vfl-inline-hint">Optional</span>
    </button>
    {open && <div className="vfl-card-body" id="vfl-record-shortcuts" data-controls-owner={controls.ownerToken || ""}
      data-controls-revision={state?.revision ?? ""} data-start-arm-mode={state?.arm?.mode || ""} data-assignment-suppressed={!!state?.assignment}>
      <div className="vfl-inline-hint">All four are off by default. Set shortcuts while recording is stopped.</div>
      {SHORTCUT_ACTIONS.map(action => {
        const row = state?.rows[action];
        const status = !controls.connected ? "Status unavailable" : !row ? "Checking shortcuts" : ({
          unset: "Not set", registered: row.dispatchEnabled ? "Active" : "Registered, waiting",
          suppressed: "Inactive while setting shortcuts", conflict: "Already in use", unavailable: "Unavailable", cleanupFailed: "Cleanup needs attention",
        })[row.status];
        return <div className="vfl-shortcut-row" key={action} id={`vfl-shortcut-${action}`} data-shortcut-status={row?.status || "unknown"} data-shortcut-effective={row?.dispatchEnabled || false}>
          <div><span className="vfl-field-label">{labels[action]}</span><output>{shortcutLabel(row?.configured || null)}</output>
            {!(row?.status === "unset" && row.configured === null) && <div className="vfl-inline-hint" role="status">{status}{row?.registered && shortcutLabel(row.registered) !== shortcutLabel(row.configured) ? ` · ${shortcutLabel(row.registered)} registered` : ""}</div>}
            {row?.reason && <div className="vfl-inline-hint">{row.reason}</div>}</div>
          <div className="vfl-actions vfl-actions-wrap"><button type="button" id={`vfl-shortcut-${action}-set`} disabled={!idle || pending || !controls.ready || !!state?.assignment} onClick={() => void begin(action)}>Set…</button>
            <button type="button" id={`vfl-shortcut-${action}-clear`} disabled={!idle || pending || !row?.configured || !!state?.assignment} onClick={() => void clear(action)}>Clear</button></div>
        </div>;
      })}
      {editing && state?.assignment && <div className="vfl-shortcut-capture">
        <label htmlFor="vfl-shortcut-capture">Set {labels[editing].toLowerCase()} shortcut</label>
        <input id="vfl-shortcut-capture" ref={input} readOnly value={candidate ? shortcutLabel(candidate) : ""} disabled={!capturing || pending || !idle}
          placeholder="Press your shortcut" aria-describedby="vfl-shortcut-capture-hint"
          onKeyDown={event => {
            if (!capturing || !state.assignment || !idle) return;
            event.preventDefault(); event.stopPropagation();
            // code is only a release bookkeeping key, never a logical VK.
            held.current.add(event.code || event.key); setHeldCount(held.current.size);
            const capture = captureShortcutEvent(event.nativeEvent); if (!capture) return;
            const epoch = captureEpoch.current, own = ++normalization.current; setNormalizing(true);
            void controls.normalize(capture).then(chord => { if (epoch === captureEpoch.current && own === normalization.current && chord) setCandidate(chord); })
              .finally(() => { if (own === normalization.current) setNormalizing(false); });
          }}
          onKeyUp={event => { event.preventDefault(); event.stopPropagation(); held.current.delete(event.code || event.key); setHeldCount(held.current.size); }}
          onBlur={() => { captureEpoch.current += 1; setCapturing(false); }} />
        <div id="vfl-shortcut-capture-hint" className="vfl-inline-hint">Use Ctrl with Shift or Alt and a letter, number, or function key. F12 is unavailable. Release every key before saving.</div>
        <div className="vfl-actions vfl-actions-wrap"><button type="button" id="vfl-shortcut-save" disabled={!candidate || heldCount > 0 || pending || normalizing} onClick={() => void finish(true)}>Save shortcut</button>
          <button type="button" id="vfl-shortcut-cancel" disabled={pending} onClick={() => void finish(false)}>Cancel</button></div>
      </div>}
      {!editing && state?.assignment && <button type="button" disabled={pending} onClick={() => void finish(false)}>Finish shortcut setup</button>}
      <button type="button" id="vfl-shortcuts-reset" disabled={!idle || pending || !controls.ready || !!state?.assignment} onClick={() => {
        setPending(true); closeCapture(); void controls.reset().then(cleared => { if (cleared) void controls.reconcile(); }).finally(() => setPending(false));
      }}>Clear all shortcuts</button>
      {state?.arm && <div className="vfl-inline-hint">{state.arm.mode === "retained" ? "Start is ready while this window stays minimized." : "Start is ready for this recording setup."}</div>}
      {state?.message && <div className="vfl-inline-hint">{state.message}</div>}
      {controls.error && <div className="vfl-error" role="alert">{controls.error} <button type="button" onClick={() => void controls.reconnect()}>Retry controls</button></div>}
    </div>}
  </section>;
}
