import type { RecordingPhase } from "../lib/recording";
import type { useRecordingPreview } from "../lib/recordingPreview";

type Props = {
  preview: ReturnType<typeof useRecordingPreview>;
  canPreview: boolean;
  recording: boolean;
  enabled: boolean;
  onEnabledChange: (enabled: boolean) => void;
  recordingPhase: RecordingPhase;
  emptyHint?: string;
};

export default function RecordingPreview({ preview, canPreview, recording, enabled, onEnabledChange, recordingPhase, emptyHint }: Props) {
  const { snapshot, image, starting, stopping, error } = preview;
  const live = snapshot.phase === "previewing" || snapshot.phase === "recording";
  const paused = ["pausing", "paused", "resuming"].includes(recordingPhase);
  const preparing = ["preparing", "countdown", "starting"].includes(recordingPhase);
  const finishing = ["stopping", "finalizing"].includes(recordingPhase);
  const blocked = paused || preparing || finishing;
  const failed = snapshot.phase === "failed" || !!error;
  const visibleImage = enabled && canPreview && live && !blocked && !starting && !stopping && !failed ? image : null;
  const waiting = enabled && canPreview && !blocked && !failed && !stopping && !visibleImage;
  const status = paused ? "Preview off during Pause and Resume"
    : preparing ? "Preparing recording…" : finishing ? "Finishing recording…"
    : stopping ? "Hiding preview…" : failed ? "Preview unavailable"
    : !enabled ? preview.hasRequest ? "Hiding preview…" : "Preview hidden"
    : !canPreview ? "Preview unavailable"
    : starting || snapshot.phase === "starting" ? "Starting preview…"
    : visibleImage ? recordingPhase === "recording" ? "Live recording preview" : recording ? "Recording preview" : "Preview only, not recording"
    : !preview.eventsReady ? "Connecting preview…" : "Waiting for source image…";

  const placeholderTitle = recordingPhase === "paused" ? "Recording paused"
    : recordingPhase === "pausing" ? "Pausing recording…" : recordingPhase === "resuming" ? "Resuming recording…"
    : preparing ? "Preparing recording…" : finishing ? "Saving recording…"
    : !enabled ? "Preview hidden" : failed ? "Preview unavailable"
    : !canPreview ? "Preview is not ready" : "Waiting for source image";
  const placeholderHint = recordingPhase === "paused" ? "Capture is off. Resume recording to continue."
    : paused ? "Preview stays off while recording changes state."
    : preparing ? "Preview will return when recording starts."
    : finishing ? "Preview stays off while the recording finishes."
    : !enabled ? "Show preview to see the selected capture source."
    : failed ? "Check the source, then retry preview."
    : !canPreview ? emptyHint || "Select a display, area or window to see what will be recorded."
    : "The selected source has not supplied a preview frame yet.";

  return <div className="vfl-record-preview" id="vfl-record-preview"
    data-preview-id={snapshot.previewId || ""} data-preview-phase={snapshot.phase}
    data-preview-epoch={snapshot.configEpoch}
    data-preview-frame-age-ms={snapshot.lastFrameAgeMs ?? ""} data-recording-phase={recordingPhase}>
    <div className="vfl-record-preview-head">
      <div className="vfl-record-preview-heading">
        <h3>Source preview</h3>
        <div id="vfl-record-preview-status" className="vfl-inline-hint" role="status" aria-live="polite">{status}</div>
      </div>
      <div className="vfl-actions vfl-actions-wrap">
        <button id={enabled ? "vfl-record-preview-stop" : "vfl-record-preview-start"} type="button"
          aria-pressed={enabled} aria-controls="vfl-record-preview-canvas" onClick={() => onEnabledChange(!enabled)}>
          {enabled ? "Hide preview" : "Show preview"}
        </button>
        {enabled && failed && !blocked && <button id="vfl-record-preview-start" type="button"
          disabled={!canPreview || !preview.eventsReady || starting || stopping} onClick={() => void preview.start()}>Retry preview</button>}
        {preview.hasRequest && error && !stopping && <button type="button"
          onClick={() => void preview.stop()}>Retry Stop preview</button>}
      </div>
    </div>
    <div id="vfl-record-preview-canvas" className="vfl-record-preview-canvas" aria-busy={waiting}>
      <canvas ref={preview.canvasRef} id="vfl-record-preview-image" className="vfl-record-preview-image"
        style={{ display: visibleImage ? "block" : "none" }} aria-hidden={!visibleImage} role="img"
        aria-label={recordingPhase === "recording" ? "Live preview of the recording source" : "Live preview of the selected capture source"} />
      {!visibleImage && <div className="vfl-record-preview-placeholder"><strong>{placeholderTitle}</strong><p>{placeholderHint}</p></div>}
    </div>
    {visibleImage && snapshot.sourceWidth > 0 && <div className="vfl-inline-hint vfl-record-preview-dimensions">{snapshot.sourceWidth} × {snapshot.sourceHeight} source pixels</div>}
    {snapshot.message && <div className="vfl-inline-hint">{snapshot.message}</div>}
    {error && <div className="vfl-error" role="alert">{error}</div>}
  </div>;
}
