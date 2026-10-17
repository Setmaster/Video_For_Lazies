import { useCallback, useEffect, useEffectEvent, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { InterpolationSettings, InterpolationOperationId, InterpolationStatus, InterpolationInstallProgress, InterpolationModelId, OutputFormat } from "../lib/types";
import { beginInterpolationInstall, bindInterpolationInstall, createInterpolationInstall, interpolationConflict, interpolationInstallActive, interpolationPublicMessage, reduceInterpolationInstall, type InterpolationInstallState } from "../lib/interpolation";

function bytes(value: number) {
  return `${(Math.max(0, value) / 1_000_000).toFixed(1)} MB`;
}

export function SmoothMotionControl({ value, format, frameRateCap, disabled, startupReady, onChange, onRemoveCap, onStatus, onBusy }: {
  value: InterpolationSettings | null;
  format: OutputFormat;
  frameRateCap: number | null;
  disabled: boolean;
  startupReady: boolean;
  onChange: (value: InterpolationSettings | null) => void;
  onRemoveCap: () => void;
  onStatus: (status: InterpolationStatus | null) => void;
  onBusy: (busy: boolean) => void;
}) {
  const sectionRef = useRef<HTMLElement | null>(null);
  const restoreCheckFocusRef = useRef<HTMLElement | null>(null);
  const [profile, setProfile] = useState<InterpolationModelId>("rife-v4.25");
  const modelId = value?.modelId ?? profile;
  const enabled = value !== null;
  const [target, setTarget] = useState<60 | 120>(60);
  const [status, setStatus] = useState<InterpolationStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const checkingRef = useRef(false);
  const [listenerReady, setListenerReady] = useState(false);
  const [install, setInstall] = useState(() => createInterpolationInstall(modelId));
  const [notice, setNotice] = useState<string | null>(null);
  const [canceling, setCanceling] = useState(false);
  const operationRef = useRef<InterpolationInstallState>(install);
  const mountedRef = useRef(false);
  const generationRef = useRef(0);
  const statusRevisionRef = useRef(0);
  const cancelRequestedRef = useRef(false);
  const busy = interpolationInstallActive(install);
  const conflict = interpolationConflict(value, format, frameRateCap);

  function applyInstall(next: InterpolationInstallState) {
    operationRef.current = next;
    setInstall(next);
    const active = interpolationInstallActive(next);
    onBusy(active || checkingRef.current);
    if (!active) {
      setCanceling(false);
      if (["ready", "canceled", "failed"].includes(next.state)) {
        window.requestAnimationFrame(() => {
          if (document.activeElement === document.body) sectionRef.current?.querySelector<HTMLButtonElement>(".vfl-smooth-motion-actions button")?.focus();
        });
      }
    }
  }

  function changeChecking(next: boolean) {
    checkingRef.current = next;
    setChecking(next);
    onBusy(next || interpolationInstallActive(operationRef.current));
  }

  const refresh = useCallback(async () => {
    if (!startupReady) return;
    const generation = generationRef.current;
    const revision = ++statusRevisionRef.current;
    const activeElement = document.activeElement;
    restoreCheckFocusRef.current = activeElement instanceof HTMLElement && sectionRef.current?.contains(activeElement) ? activeElement : null;
    changeChecking(true);
    setStatus(null);
    onStatus(null);
    try {
      const next = await invoke<InterpolationStatus>("interpolation_status", { modelId });
      if (!mountedRef.current || generation !== generationRef.current || revision !== statusRevisionRef.current || next.modelId !== modelId) return;
      setStatus(next);
      onStatus(next);
    } catch (error) {
      if (!mountedRef.current || generation !== generationRef.current || revision !== statusRevisionRef.current) return;
      setNotice(interpolationPublicMessage(error, "Smooth motion could not be checked. Try checking again."));
    } finally {
      if (mountedRef.current && generation === generationRef.current && revision === statusRevisionRef.current) {
        changeChecking(false);
        window.requestAnimationFrame(() => {
          const previousControl = restoreCheckFocusRef.current;
          const control = previousControl?.isConnected && !previousControl.hasAttribute("disabled")
            ? previousControl
            : operationRef.current.state === "ready"
              ? sectionRef.current?.querySelector<HTMLButtonElement>(".vfl-smooth-motion-actions button")
              : null;
          if (document.activeElement === document.body) control?.focus();
        });
      }
    }
  }, [modelId, onStatus, startupReady]);

  const receiveProgress = useEffectEvent((event: InterpolationInstallProgress) => {
    const previous = operationRef.current;
    const next = reduceInterpolationInstall(previous, event);
    if (next === previous) return;
    applyInstall(next);
    if (next.state === "ready") {
      setNotice(null);
      void refresh(); // Ready is authoritative only after the resolver's self-test.
    } else if (next.state === "canceled") {
      setNotice("Installation canceled. You can retry when ready.");
    } else if (next.state === "failed") {
      setNotice(next.message || "Installation did not finish. Check your connection and free space, then retry.");
    }
  });

  useEffect(() => {
    mountedRef.current = true;
    generationRef.current += 1;
    setListenerReady(false);
    setNotice(null);
    setStatus(null);
    onStatus(null);
    changeChecking(enabled && !startupReady);
    applyInstall(createInterpolationInstall(modelId));
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void (async () => {
      try {
        const release = await listen<InterpolationInstallProgress>("interpolation-install-progress", ({ payload }) => receiveProgress(payload));
        if (disposed) { release(); return; }
        unlisten = release;
        setListenerReady(true);
        if (enabled && startupReady) await refresh();
      } catch {
        if (!disposed) setNotice("Installation updates could not start. Restart the app to try again.");
      }
    })();
    return () => {
      disposed = true;
      mountedRef.current = false;
      generationRef.current += 1;
      checkingRef.current = false;
      onBusy(false);
      unlisten?.();
      const current = operationRef.current;
      if (interpolationInstallActive(current) && current.operationId) {
        void invoke("cancel_interpolation_install", { operationId: current.operationId }).catch(() => {});
      }
    };
  }, [modelId, enabled, startupReady]);

  async function startInstall() {
    if (disabled || !listenerReady || checking || interpolationInstallActive(operationRef.current)) return;
    const generation = generationRef.current;
    cancelRequestedRef.current = false;
    setNotice(null);
    setStatus(null);
    onStatus(null);
    statusRevisionRef.current += 1;
    applyInstall(beginInterpolationInstall(modelId));
    try {
      const operationId = await invoke<InterpolationOperationId>("start_interpolation_install", { modelId });
      if (!mountedRef.current || generation !== generationRef.current) {
        void invoke("cancel_interpolation_install", { operationId }).catch(() => {});
        return;
      }
      const next = bindInterpolationInstall(operationRef.current, operationId);
      applyInstall(next);
      if (next.state === "ready") void refresh();
      if (next.state === "failed") setNotice(next.message || "Installation did not finish. Check your connection and free space, then retry.");
      if (next.state === "canceled") setNotice("Installation canceled. You can retry when ready.");
      if (cancelRequestedRef.current && interpolationInstallActive(next)) {
        try { await invoke("cancel_interpolation_install", { operationId }); }
        catch { setCanceling(false); setNotice("Cancellation could not be confirmed. Try Cancel again."); }
      }
    } catch (error) {
      if (!mountedRef.current || generation !== generationRef.current) return;
      applyInstall({ ...operationRef.current, state: "failed", pendingEvents: [] });
      setNotice(interpolationPublicMessage(error, "Installation could not start or finish. Check your connection and free space, then retry."));
    }
  }

  async function cancelInstall() {
    if (!interpolationInstallActive(operationRef.current)) return;
    cancelRequestedRef.current = true;
    setCanceling(true);
    const operationId = operationRef.current.operationId;
    if (!operationId) return; // startInstall sends cancellation as soon as the ID arrives.
    try { await invoke("cancel_interpolation_install", { operationId }); }
    catch { if (mountedRef.current) { setCanceling(false); setNotice("Cancellation could not be confirmed. Try Cancel again."); } }
  }

  async function removeModel() {
    if (disabled || busy || checking || status?.bundled || !status || (status.state !== "ready" && status.installedBytes <= 0)) return;
    const generation = generationRef.current;
    changeChecking(true);
    setStatus(null);
    onStatus(null);
    try {
      const next = await invoke<InterpolationStatus>("remove_interpolation_model", { modelId });
      if (!mountedRef.current || generation !== generationRef.current) return;
      setStatus(next); onStatus(next); setNotice("Downloaded model removed.");
    } catch (error) {
      if (mountedRef.current && generation === generationRef.current) setNotice(interpolationPublicMessage(error, "The model could not be removed. Wait for active work to finish, then retry."));
    } finally {
      if (mountedRef.current && generation === generationRef.current) changeChecking(false);
    }
  }

  const phaseText = canceling ? "Canceling installation" : install.state === "verifying" ? "Verifying download" : install.state === "testing" ? "Testing Smooth motion" : "Downloading frame interpolation";
  const downloaded = install.totalBytes > 0 ? Math.min(100, Math.floor(install.completedBytes / install.totalBytes * 100)) : 0;
  return (
    <section ref={sectionRef} className="vfl-smooth-motion" aria-labelledby="vfl-smooth-motion-title">
      <label className="vfl-check vfl-check-card" id="vfl-smooth-motion-title">
        <input id="vfl-smooth-motion" type="checkbox" checked={value !== null} disabled={disabled || !startupReady || busy || checking || (format === "mp3" && value === null)}
          onChange={(event) => {
            if (event.currentTarget.checked) onChange({ targetFps: target, modelId });
            else {
              if (value) { setTarget(value.targetFps); setProfile(value.modelId); }
              onChange(null);
            }
          }} />
        <span>Enable Smooth motion</span>
      </label>
      <div className="vfl-inline-hint">{format === "mp3" ? "Available for MP4 and WebM video exports." : value ? "AI creates frames for smoother motion. Duration and normal-speed audio stay the same." : "Off. Keeps the source motion without creating frames."}</div>
      {value ? <>
        <div className="vfl-segmented" role="group" aria-label="Smooth motion frame rate">
          {([60, 120] as const).map((fps) => <button type="button" key={fps} className={value.targetFps === fps ? "active" : ""} aria-pressed={value.targetFps === fps} disabled={disabled || !startupReady || busy || checking || format === "mp3"}
            onClick={() => { setTarget(fps); onChange({ ...value, targetFps: fps }); }}>{fps} fps</button>)}
        </div>
        <div className="vfl-field">
          <label htmlFor="vfl-smooth-motion-model">Motion model</label>
          <select id="vfl-smooth-motion-model" value={modelId} disabled={disabled || !startupReady || busy || checking || format === "mp3"} onChange={(event) => {
            const selected = event.currentTarget.value as InterpolationModelId;
            onStatus(null); setProfile(selected); onChange({ ...value, modelId: selected });
          }}>
            <option value="rife-v4.25">Standard (4.25)</option>
            <option value="rife-v4.26">Alternative (4.26)</option>
          </select>
        </div>
        <div className="vfl-inline-hint" role="status" aria-live="polite" data-smoke-id="interpolation-status">
          {checking ? "Checking Smooth motion..." : busy ? phaseText : status?.state === "ready" ? `Ready${status.bundled ? ". Included with the app." : ". Available offline."}` : status?.message || "Check or install frame interpolation before exporting."}
        </div>
        {busy ? <div>
          <div className="vfl-progress" role="progressbar" aria-label={phaseText} aria-valuemin={0} aria-valuemax={100} aria-valuenow={install.state === "downloading" ? downloaded : undefined} aria-valuetext={phaseText}>
            <div className="vfl-progress-meta"><span>{phaseText}</span><span>{bytes(install.completedBytes)} / {bytes(install.totalBytes)}</span></div>
            <div className="vfl-progress-bar"><div className="vfl-progress-fill" style={{ width: `${install.state === "downloading" ? downloaded : 100}%` }} /></div>
          </div>
          <button type="button" onClick={() => void cancelInstall()} disabled={canceling}>{canceling ? "Canceling..." : "Cancel installation"}</button>
        </div> : <div className="vfl-smooth-motion-actions">
          {status?.state !== "ready" && status?.state !== "unavailable" ? <button type="button" onClick={() => void startInstall()} disabled={disabled || !startupReady || !listenerReady || checking}>
            {install.state === "failed" || install.state === "canceled" || status?.state === "failed" ? "Retry installation" : "Install frame interpolation"}{status?.downloadBytes ? ` (${bytes(status.downloadBytes)})` : ""}
          </button> : null}
          <button type="button" onClick={() => { setNotice(null); void refresh(); }} disabled={disabled || !startupReady || !listenerReady || checking}>Check again</button>
          {status && !status.bundled && (status.state === "ready" || status.installedBytes > 0) ? <button type="button" onClick={() => void removeModel()} disabled={disabled || checking}>Remove downloaded model</button> : null}
        </div>}
        {notice ? <div className={install.state === "failed" ? "vfl-error" : "vfl-inline-hint"} role="status">{notice}</div> : null}
        {conflict ? <div className="vfl-error" role="alert">{conflict}{format !== "mp3" ? <button type="button" onClick={onRemoveCap} disabled={disabled || busy}>Remove frame-rate cap</button> : null}</div> : null}
        <div className="vfl-inline-hint">Use Export exact sample in Advanced to render a short result. Source playback does not show AI frames.</div>
      </> : null}
    </section>
  );
}
