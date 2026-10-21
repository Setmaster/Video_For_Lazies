import type { useRecordingPreview } from "../lib/recordingPreview";

type Props = { preview: ReturnType<typeof useRecordingPreview>; canPreview: boolean; recording: boolean };
export default function RecordingPreview({ preview, canPreview, recording }: Props) {
  const { snapshot, image, starting, stopping, error } = preview;
  const live = snapshot.phase === "previewing" || snapshot.phase === "recording";
  const status = stopping ? "Stopping preview…" : starting || snapshot.phase === "starting" ? "Starting preview…"
    : snapshot.phase === "failed" ? "Preview unavailable" : live ? recording ? "Live recording preview" : "Preview only · Not recording" : "Preview is off";
  return <div className="vfl-record-preview" id="vfl-record-preview"
    data-preview-id={snapshot.previewId || ""} data-preview-phase={snapshot.phase}
    data-preview-epoch={snapshot.configEpoch} data-preview-sequence={image?.sequence || 0}
    data-preview-frame-age-ms={snapshot.lastFrameAgeMs ?? ""}>
    {image && <img id="vfl-record-preview-image" className="vfl-record-preview-image" src={image.url}
      alt={recording ? "Live preview of the recording source" : "Live preview of the selected capture source"} />}
    <div id="vfl-record-preview-status" className="vfl-inline-hint" role="status" aria-live="polite">{status}</div>
    {live && !image && <div className="vfl-inline-hint">Waiting for the selected source image…</div>}
    {live && snapshot.sourceWidth > 0 && <div className="vfl-inline-hint">{snapshot.sourceWidth} × {snapshot.sourceHeight} source pixels</div>}
    {snapshot.message && <div className="vfl-inline-hint">{snapshot.message}</div>}
    {error && <div className="vfl-error" role="alert">{error}</div>}
    <div className="vfl-actions vfl-actions-wrap">
      {live || starting || snapshot.phase === "starting" ? <button id="vfl-record-preview-stop" type="button" disabled={stopping} onClick={() => void preview.stop()}>Stop preview</button>
        : <button id="vfl-record-preview-start" type="button" disabled={!canPreview || !preview.eventsReady || starting || stopping}
          onClick={() => void preview.start()}>{snapshot.phase === "failed" || error ? "Retry preview" : "Preview"}</button>}
      {!live && !starting && preview.hasRequest && error && <button type="button" disabled={stopping} onClick={() => void preview.stop()}>Retry Stop preview</button>}
    </div>
  </div>;
}
