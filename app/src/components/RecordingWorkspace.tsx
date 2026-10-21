import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { useRecording } from "../lib/recording";
import type { CaptureSource, RecordingConfig, RecordingPhase, RecordingRegion, RecordingStateChange } from "../lib/recording";
import { captureSourceKey, formatRecordingDuration, isRecordingRegionWithinDisplay, recordingSourceProblem, recordingWindowProblem } from "../lib/recordingState";
import { useRecordingPreview } from "../lib/recordingPreview";
import type { PreviewHandoff } from "../lib/recordingPreview";
import RecordingPreview from "./RecordingPreview";
import "./RecordingWorkspace.css";

type Props = {
  visible: boolean;
  busy?: boolean;
  onRecordingStateChange?: RecordingStateChange;
  onPreviewAdmissionBusyChange?: (busy: boolean) => void;
  onEditVideo?: (path: string) => void | Promise<void>;
};

const phaseTitles: Record<RecordingPhase, string> = {
  idle: "Ready to record", preparing: "Preparing recording", countdown: "Recording starts in",
  starting: "Starting recording", recording: "Recording", stopping: "Stopping recording",
  finalizing: "Saving MP4", completed: "Recording saved", recoverable: "Recording needs recovery",
  failed: "Recording could not finish",
};

export default function RecordingWorkspace({ visible, busy = false, onRecordingStateChange, onPreviewAdmissionBusyChange, onEditVideo }: Props) {
  const recorder = useRecording(onRecordingStateChange);
  const { snapshot, capabilities, sources, windows, windowsTruncated, audioDevices, pending, active } = recorder;
  const [captureMode, setCaptureMode] = useState<"area" | "display" | "window">("area");
  const [displayId, setDisplayId] = useState("");
  const [windowId, setWindowId] = useState("");
  const resumePreview = useRef<PreviewHandoff | null>(null);
  const [region, setRegion] = useState<RecordingRegion | null>(null);
  const [regionDisplayGeometry, setRegionDisplayGeometry] = useState("");
  const [systemAudio, setSystemAudio] = useState(false);
  const [microphone, setMicrophone] = useState(false);
  const [systemAudioId, setSystemAudioId] = useState("");
  const [microphoneId, setMicrophoneId] = useState("");
  const [showCursor, setShowCursor] = useState(true);
  const [quality, setQuality] = useState<"standard" | "high">("standard");
  const [frameRate, setFrameRate] = useState("30");
  const [countdown, setCountdown] = useState("3");
  const [outputDirectory, setOutputDirectory] = useState("");
  const restoredSession = useRef<string | null>(null);
  const startedRequest = useRef<string | null>(null);
  const isArea = captureMode === "area";
  const isWindow = captureMode === "window";
  const display = sources.find((source) => source.id === displayId);
  const displayGeometry = display ? `${display.id}:${display.x}:${display.y}:${display.width}:${display.height}` : "";
  const validRegion = isRecordingRegionWithinDisplay(region, display) && regionDisplayGeometry === displayGeometry;
  const candidateSource: CaptureSource | null = isWindow ? (windowId ? { kind: "window", windowId } : null) : isArea ? (region ? { kind: "region", ...region } : null) : display ? {
    kind: "region", displayId: display.id, x: display.x, y: display.y, width: display.width, height: display.height,
  } : null;
  const sourceProblem = isWindow ? recordingWindowProblem(windowId, windows) : isArea && region && regionDisplayGeometry !== displayGeometry
    ? "The display changed. Select your area again."
    : recordingSourceProblem(candidateSource?.kind === "region" ? candidateSource : null, display);
  const selectedSource = sourceProblem ? null : candidateSource;
  const systems = audioDevices.filter((device) => device.kind === "system");
  const microphones = audioDevices.filter((device) => device.kind === "microphone");
  const audioReady = (!systemAudio || systems.some((device) => device.id === systemAudioId)) &&
    (!microphone || microphones.some((device) => device.id === microphoneId));
  const disabled = busy || active || recorder.selecting || pending !== null || !capabilities?.available;
  const preview = useRecordingPreview({ visible, source: active && snapshot.config ? snapshot.config.source : selectedSource,
    showCursor: active && snapshot.config ? snapshot.config.showCursor : showCursor, recordingPhase: snapshot.phase, recordingSessionId: snapshot.sessionId,
    onAdmissionBusyChange: onPreviewAdmissionBusyChange });
  const canPreview = visible && !!capabilities?.available && !busy && !recorder.selecting && pending === null &&
    (!active ? !!selectedSource : snapshot.phase === "recording");
  const ready = !preview.starting && !preview.stopping && preview.snapshot.phase !== "starting" && !disabled && recorder.connected && selectedSource !== null && audioReady && outputDirectory.trim() !== "";
  const canCancel = ["preparing", "countdown", "starting"].includes(snapshot.phase);
  const canStop = snapshot.phase === "recording" || canCancel;
  const savedPath = snapshot.phase === "completed" ? snapshot.outputPath : null;
  const statusTitle = capabilities?.available === false
    ? "Recording is unavailable on this system"
    : active || snapshot.phase !== "idle" ? phaseTitles[snapshot.phase]
    : selectedSource ? "Ready to record" : isWindow ? "Choose a window or game" : isArea ? "No area selected" : "Choose a display";

  useEffect(() => {
    if (!displayId && sources.length) setDisplayId(sources[0].id);
  }, [displayId, sources]);
  useEffect(() => {
    if (!systemAudioId && systems.length) setSystemAudioId(systems[0].id);
    if (!microphoneId && microphones.length) setMicrophoneId(microphones[0].id);
  }, [systemAudioId, microphoneId, systems, microphones]);
  useEffect(() => {
    if (capabilities?.defaultOutputDirectory) setOutputDirectory((current) => current || capabilities.defaultOutputDirectory || "");
  }, [capabilities]);
  useEffect(() => {
    const config = snapshot.config;
    if (!active || !snapshot.sessionId || !config) return;
    const source = config.source;
    const restoredDisplay = source.kind === "region" ? sources.find((item) => item.id === source.displayId) : undefined;
    const geometry = restoredDisplay ? `${restoredDisplay.id}:${restoredDisplay.x}:${restoredDisplay.y}:${restoredDisplay.width}:${restoredDisplay.height}` : "";
    const restoreKey = `${snapshot.sessionId}:${captureSourceKey(source)}:${geometry}`;
    if (restoredSession.current === restoreKey) return;
    restoredSession.current = restoreKey;
    if (config.requestId === startedRequest.current) return;
    if (source.kind === "window") { setCaptureMode("window"); setWindowId(source.windowId); }
    else {
      setDisplayId(source.displayId); setRegion(source); setRegionDisplayGeometry(geometry);
      setCaptureMode(restoredDisplay && source.x === restoredDisplay.x && source.y === restoredDisplay.y && source.width === restoredDisplay.width && source.height === restoredDisplay.height ? "display" : "area");
    }
    setSystemAudio(config.systemAudioDeviceId !== null); setMicrophone(config.microphoneDeviceId !== null);
    setSystemAudioId(config.systemAudioDeviceId || ""); setMicrophoneId(config.microphoneDeviceId || "");
    setShowCursor(config.showCursor); setQuality(config.quality); setFrameRate(String(config.fps));
    setCountdown(String(config.countdownSeconds)); setOutputDirectory(config.outputDirectory);
  }, [active, snapshot, sources]);

  useEffect(() => {
    const intent = resumePreview.current;
    if (!intent) return;
    if (!preview.isCurrent(intent) || !active) { resumePreview.current = null; return; }
    if (snapshot.phase === "recording") { resumePreview.current = null; void preview.start(); }
  }, [active, snapshot.phase, preview]);

  function changeCaptureMode(mode: "area" | "display" | "window") {
    resumePreview.current = null;
    void preview.stop();
    setCaptureMode(mode);
  }

  async function refreshDevices() {
    resumePreview.current = null;
    if (await preview.stop()) await recorder.refreshDevices();
  }


  async function chooseFolder() {
    await recorder.run("folder", async () => {
      const chosen = await open({ directory: true, multiple: false, title: "Save recordings to", defaultPath: outputDirectory || undefined });
      if (typeof chosen === "string") setOutputDirectory(chosen);
    });
  }

  async function selectArea() {
    if (disabled || !display) return;
    const geometry = displayGeometry;
    resumePreview.current = null;
    if (!await preview.stop()) return;
    await recorder.run("selection", async () => {
      const chosen = await invoke<RecordingRegion | null>("recording_select_region", { displayId: display.id, requestId: crypto.randomUUID() });
      if (chosen !== null) {
        const problem = recordingSourceProblem(chosen, display);
        if (problem) throw new Error(problem);
        setRegion(chosen);
        setRegionDisplayGeometry(geometry);
      }
    }, true);
  }

  async function start() {
    if (!ready || !selectedSource) return;
    const config: RecordingConfig = {
      requestId: crypto.randomUUID(), source: selectedSource, fps: Number(frameRate), quality,
      countdownSeconds: Number(countdown), showCursor,
      systemAudioDeviceId: systemAudio ? systemAudioId : null,
      microphoneDeviceId: microphone ? microphoneId : null, outputDirectory,
    };
    startedRequest.current = config.requestId;
    const intent = preview.detachForRecording();
    const result = await recorder.command("recording_start", { config });
    await preview.finishHandoff();
    if (result && intent.resume && preview.isCurrent(intent) && ["preparing", "countdown", "starting", "recording"].includes(result.phase)) {
      resumePreview.current = intent;
      if (result.phase === "recording") { resumePreview.current = null; void preview.start(); }
    }
  }

  async function discardRecovery(sessionId: string) {
    await recorder.run("discard", async () => {
      const approved = await confirm("Permanently discard this unfinished recording? This cannot be undone.", {
        title: "Discard recording", kind: "warning", okLabel: "Discard recording", cancelLabel: "Keep recording files",
      });
      if (!approved) return;
      await invoke("recording_discard_recovery", { sessionId });
      await recorder.refreshSnapshot();
    }, true);
  }

  return (
    <div className="vfl-record-workspace">
      <div className="vfl-split">
        <section className="vfl-pane-left" aria-labelledby="vfl-record-capture-title">
          <div className="vfl-record-toolbar">
            <h2 id="vfl-record-capture-title" className="vfl-section-title">Capture</h2>
            <fieldset className="vfl-record-modes" disabled={disabled}>
              <legend className="vfl-sr-only">Capture mode</legend>
              <label className={isArea ? "is-selected" : ""}>
                <input type="radio" name="vfl-record-capture-mode" value="area" checked={isArea} onChange={() => changeCaptureMode("area")} />Area
              </label>
              <label className={captureMode === "display" ? "is-selected" : ""}>
                <input type="radio" name="vfl-record-capture-mode" value="display" checked={captureMode === "display"} onChange={() => changeCaptureMode("display")} />Full screen
              </label>
              <label className={isWindow ? "is-selected" : ""}>
                <input type="radio" name="vfl-record-capture-mode" value="window" checked={isWindow} onChange={() => changeCaptureMode("window")} />Window or game
              </label>
            </fieldset>
          </div>
          <div className="vfl-record-source-row">
            <div className="vfl-field">
              {isWindow ? <>
                <label htmlFor="vfl-record-window">Window or game</label>
                <select id="vfl-record-window" value={windowId} disabled={disabled}
                  onChange={(event) => { void preview.stop(); setWindowId(event.currentTarget.value); }}>
                  <option value="">Choose a window or game</option>
                  {windowId && !windows.some((target) => target.id === windowId) && <option value={windowId}>Selected window unavailable</option>}
                  {windows.map((target) => <option key={target.id} value={target.id}>{target.name} · {target.processName}{!target.available ? " · Unavailable" : ""}</option>)}
                </select>
              </> : <>
              <label htmlFor="vfl-record-display">Display</label>
              <select id="vfl-record-display" value={displayId} disabled={disabled || sources.length === 0}
                onChange={(event) => { void preview.stop(); setDisplayId(event.currentTarget.value); setRegion(null); }}>
                {!display && <option value={displayId}>{displayId ? "Selected display unavailable" : "No displays available"}</option>}
                {sources.map((source) => <option key={source.id} value={source.id}>{source.name} · {source.width} × {source.height}</option>)}
              </select></>}
            </div>
            <button type="button" disabled={busy || active || recorder.selecting || pending !== null} onClick={() => void refreshDevices()}>Refresh devices</button>
          </div>
          {isWindow && <div id="vfl-record-window-method" className="vfl-inline-hint">Windows capture (WGC) · Windowed and borderless games. Protected or exclusive full-screen content may be unavailable. Preview follows resizing; recording stops and saves if the window changes size.{windowsTruncated ? " Window list limited to 128 entries. Close unused windows and refresh if needed." : ""}</div>}
          <div className={`vfl-record-stage ${active ? "is-recording-active" : ""} ${preview.image ? "has-preview" : ""}`}>
            <div className="vfl-record-empty">
              {(!preview.hasRequest && !preview.starting) && <svg className="vfl-record-illustration" viewBox="0 0 200 152" fill="none" aria-hidden="true">
                <rect x="24" y="18" width="152" height="100" rx="8" />
                <path d="M24 100h152M100 118v18m-27 0h54" />
                {isArea ? <>
                  <rect className="vfl-record-area-outline" x="53" y="38" width="94" height="48" rx="2" />
                  <path className="vfl-record-area-corners" d="M48 44V33h12m80 0h12v11M48 80v11h12m80 0h12V80" />
                </> : <rect className="vfl-record-display-outline" x="33" y="27" width="134" height="64" rx="3" />}
              </svg>}
              <RecordingPreview preview={preview} canPreview={canPreview} recording={active} />
              <h3 aria-live="polite">{statusTitle}</h3>
              {snapshot.phase === "countdown" && <div className="vfl-record-countdown" role="timer" aria-live="polite" aria-atomic="true" aria-label="Seconds until recording">{snapshot.countdownRemaining ?? "…"}</div>}
              {active && snapshot.phase !== "countdown" && <div className="vfl-record-timer" role="timer" aria-label="Recording duration">{formatRecordingDuration(snapshot.elapsedMs)}</div>}
              {active && snapshot.config && <p>{snapshot.resolvedGeometry ? `${snapshot.resolvedGeometry.width} × ${snapshot.resolvedGeometry.height} pixels · ` : ""}{snapshot.config.fps} fps<br />{snapshot.config.systemAudioDeviceId ? "System audio on" : "System audio off"} · {snapshot.config.microphoneDeviceId ? "Microphone on" : "Microphone off"}</p>}
              {!active && !savedPath && <p>{selectedSource ? selectedSource.kind === "region" ? `${selectedSource.width} × ${selectedSource.height} pixels · ${frameRate} fps` : `Selected window · ${frameRate} fps` : isWindow ? "Select one window above. Other windows are not captured." : isArea ? "Select a region within one display." : "Select one full display above."}</p>}
              {!active && candidateSource && sourceProblem && <p className="vfl-record-source-hint">{sourceProblem}</p>}
              {selectedSource?.kind === "region" && (selectedSource.width % 2 !== 0 || selectedSource.height % 2 !== 0) && <p className="vfl-record-source-hint">Your exact area is captured. MP4 adds a black edge to make the output dimensions even.</p>}
              {!active && isArea && capabilities?.available && <button type="button" disabled={disabled || !display} onClick={() => void selectArea()}>{pending === "selection" ? "Selecting area…" : validRegion ? "Change area" : "Select area"}</button>}
              {active && <p className="vfl-record-source-hint">{snapshot.phase === "recording" ? "Screen capture is active. Stop to save your MP4." : "Keep the app open while recording finishes."}</p>}
              {savedPath && <>
                <p className="vfl-record-path">{savedPath}</p>
                <div className="vfl-actions vfl-actions-wrap">
                  {onEditVideo && <button type="button" className="primary" disabled={busy || pending !== null} onClick={() => void recorder.run("edit", async () => onEditVideo(savedPath))}>Edit video</button>}
                  <button type="button" disabled={pending !== null} onClick={() => void recorder.run("open", () => openPath(savedPath))}>Open video</button>
                </div>
              </>}
              {snapshot.message && <p className="vfl-record-source-hint">{snapshot.message}</p>}
              {capabilities?.available === false && <p className="vfl-record-source-hint">{capabilities.reason || "Screen recording is supported on Windows only. Editing is still available."}</p>}
              {!capabilities && <p className="vfl-record-source-hint">{pending === "refresh" ? "Checking recording support…" : "Recording support could not be checked. Refresh devices to retry."}</p>}
            </div>
          </div>
          {(recorder.error || recorder.statusError) && <div className="vfl-error" role="alert">
            {recorder.error && <div>{recorder.error}</div>}
            {recorder.statusError && <div>{recorder.statusError} <button type="button" onClick={() => void recorder.refreshSnapshot()}>Retry status</button></div>}
          </div>}
        </section>

        <aside className="vfl-rail" aria-label="Recording settings">
          <section className="vfl-card is-open" aria-labelledby="vfl-record-audio-title">
            <div className="vfl-record-card-head"><h2 id="vfl-record-audio-title" className="vfl-card-title">Audio</h2></div>
            <div className="vfl-card-body">
              <label className="vfl-check vfl-check-card"><input type="checkbox" checked={systemAudio} disabled={disabled || (!systemAudio && systems.length === 0)} onChange={(event) => setSystemAudio(event.currentTarget.checked)} /><span>System audio</span></label>
              {systemAudio && <div className="vfl-field"><label htmlFor="vfl-record-system-device">System audio device</label><select id="vfl-record-system-device" value={systemAudioId} disabled={disabled} onChange={(event) => setSystemAudioId(event.currentTarget.value)}>
                {!systems.some((device) => device.id === systemAudioId) && <option value={systemAudioId}>Selected device unavailable</option>}
                {systems.map((device) => <option key={device.id} value={device.id}>{device.name}</option>)}
              </select></div>}
              <label className="vfl-check vfl-check-card"><input type="checkbox" checked={microphone} disabled={disabled || (!microphone && microphones.length === 0)} onChange={(event) => setMicrophone(event.currentTarget.checked)} /><span>Microphone</span></label>
              {microphone && <div className="vfl-field"><label htmlFor="vfl-record-microphone-device">Microphone device</label><select id="vfl-record-microphone-device" value={microphoneId} disabled={disabled} onChange={(event) => setMicrophoneId(event.currentTarget.value)}>
                {!microphones.some((device) => device.id === microphoneId) && <option value={microphoneId}>Selected device unavailable</option>}
                {microphones.map((device) => <option key={device.id} value={device.id}>{device.name}</option>)}
              </select></div>}
              <div className="vfl-inline-hint">Microphone is off by default. Audio starts only when you record.</div>
              {capabilities?.available && (!systems.length || !microphones.length) && <div className="vfl-inline-hint">{!systems.length ? "No system audio device found. " : ""}{!microphones.length ? "No microphone found." : ""}</div>}
            </div>
          </section>
          <section className="vfl-card is-open" aria-labelledby="vfl-record-video-title">
            <div className="vfl-record-card-head"><h2 id="vfl-record-video-title" className="vfl-card-title">Video</h2></div>
            <div className="vfl-card-body">
              <div className="vfl-field"><label htmlFor="vfl-record-quality">Quality</label><select id="vfl-record-quality" value={quality} disabled={disabled} aria-describedby="vfl-record-quality-hint" onChange={(event) => setQuality(event.currentTarget.value as "standard" | "high")}><option value="standard">Standard</option><option value="high">High</option></select><div id="vfl-record-quality-hint" className="vfl-inline-hint">High keeps more detail and uses more disk space.</div></div>
              <div className="vfl-inline-hint">HDR displays are converted to SDR.</div>
              <div className="vfl-row2">
                <div className="vfl-field"><label htmlFor="vfl-record-frame-rate">Frame rate</label><select id="vfl-record-frame-rate" value={frameRate} disabled={disabled} onChange={(event) => setFrameRate(event.currentTarget.value)}>{[24, 25, 30, 50, 60].map((fps) => <option key={fps} value={String(fps)}>{fps} fps</option>)}</select></div>
                <div className="vfl-field"><label htmlFor="vfl-record-countdown">Countdown</label><select id="vfl-record-countdown" value={countdown} disabled={disabled} aria-describedby="vfl-record-countdown-hint" onChange={(event) => setCountdown(event.currentTarget.value)}><option value="0">None</option><option value="3">3 seconds</option><option value="5">5 seconds</option></select></div>
              </div>
              <div id="vfl-record-countdown-hint" className="vfl-inline-hint">None starts capture as soon as preparation finishes; 3 or 5 seconds gives you time to get ready.</div>
              <label className="vfl-check"><input type="checkbox" checked={showCursor} disabled={disabled} onChange={(event) => { void preview.stop(); setShowCursor(event.currentTarget.checked); }} /><span>Show cursor</span></label>
            </div>
          </section>
          <section className="vfl-card is-open" aria-labelledby="vfl-record-save-title">
            <div className="vfl-record-card-head"><h2 id="vfl-record-save-title" className="vfl-card-title">Save</h2></div>
            <div className="vfl-card-body">
              <div className="vfl-record-format"><span className="vfl-field-label">Format</span><span>MP4</span></div>
              <div className="vfl-field"><label htmlFor="vfl-record-output-directory">Output folder</label><input id="vfl-record-output-directory" value={outputDirectory} readOnly placeholder="Choose a local folder" /><button type="button" disabled={disabled} onClick={() => void chooseFolder()}>Choose folder</button></div>
              <div className="vfl-inline-hint">Temporary files can be larger than the saved MP4, especially with audio. Keep extra free space.</div>
              {outputDirectory && <button type="button" disabled={busy || active || pending !== null} onClick={() => void recorder.run("open-folder", () => openPath(outputDirectory))}>Open folder</button>}
            </div>
          </section>
          {snapshot.recoveries.length > 0 && <section className="vfl-card is-open" aria-labelledby="vfl-record-recovery-title">
            <div className="vfl-record-card-head"><h2 id="vfl-record-recovery-title" className="vfl-card-title">Unfinished recordings</h2></div>
            <div className="vfl-card-body"><div className="vfl-inline-hint">Recovery may omit the unfinished end. Files are kept until you recover or discard them.</div>
              {snapshot.recoveries.map((recovery) => <div key={recovery.sessionId} className="vfl-record-recovery">
                <div>{recovery.createdAt}</div><div className="vfl-inline-hint">{recovery.message}</div>
                <div className="vfl-actions vfl-actions-wrap"><button type="button" disabled={disabled || !recorder.connected} onClick={() => void recorder.command("recording_recover", { sessionId: recovery.sessionId, outputDirectory: outputDirectory || null })}>Recover</button><button type="button" disabled={disabled} onClick={() => void discardRecovery(recovery.sessionId)}>Discard…</button></div>
              </div>)}
            </div>
          </section>}
        </aside>
      </div>
      <footer className="vfl-footer vfl-record-footer">
        <div className="vfl-footer-main">
          <div className="vfl-record-footer-copy">
            <div className="vfl-record-footer-line"><span className="vfl-footer-kicker">Record</span><span role="status" aria-live="polite">{pending === "selection" ? "Select an area on your display" : capabilities?.available === false ? "Recording unavailable" : phaseTitles[snapshot.phase]}</span></div>
            <div className="vfl-inline-hint">{active ? `${formatRecordingDuration(snapshot.elapsedMs)} · ${(snapshot.bytesWritten / 1048576).toFixed(1)} MB written` : busy ? "Finish the current app activity before recording." : !capabilities?.available ? capabilities?.reason || "Checking recording support." : sourceProblem ? sourceProblem : !audioReady ? "Choose an available audio device or turn that source off." : !outputDirectory ? "Choose a local output folder." : "Recording begins only after Start recording and the countdown."}</div>
          </div>
          <div className="vfl-footer-actions">
            {canStop && snapshot.sessionId ? <button type="button" className="danger vfl-record-start" disabled={pending !== null} onClick={() => { resumePreview.current = null; void preview.stop(); void recorder.command("recording_stop", { sessionId: snapshot.sessionId }); }}>{canCancel ? "Cancel recording" : "Stop and save"}</button>
              : <button type="button" className="primary vfl-record-start" disabled={!ready} onClick={() => void start()}>{snapshot.phase === "stopping" || snapshot.phase === "finalizing" ? "Saving…" : pending === "recording_start" ? "Preparing…" : "Start recording"}</button>}
          </div>
        </div>
      </footer>
    </div>
  );
}
