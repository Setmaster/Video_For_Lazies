import { useState } from "react";
import "./RecordingWorkspace.css";

export default function RecordingWorkspace() {
  const [captureMode, setCaptureMode] = useState<"area" | "display">("area");
  const [systemAudio, setSystemAudio] = useState(false);
  const [microphone, setMicrophone] = useState(false);
  const [showCursor, setShowCursor] = useState(true);
  const [quality, setQuality] = useState("standard");
  const [frameRate, setFrameRate] = useState("30");
  const [countdown, setCountdown] = useState("3");
  const isArea = captureMode === "area";

  return (
    <div className="vfl-record-workspace">
      <div className="vfl-split">
        <section className="vfl-pane-left" aria-labelledby="vfl-record-capture-title">
          <div className="vfl-record-toolbar">
            <h2 id="vfl-record-capture-title" className="vfl-section-title">Capture</h2>
            <fieldset className="vfl-record-modes">
              <legend className="vfl-sr-only">Capture mode</legend>
              <label className={isArea ? "is-selected" : ""}>
                <input
                  type="radio"
                  name="vfl-record-capture-mode"
                  value="area"
                  checked={isArea}
                  onChange={() => setCaptureMode("area")}
                />
                Area
              </label>
              <label className={!isArea ? "is-selected" : ""}>
                <input
                  type="radio"
                  name="vfl-record-capture-mode"
                  value="display"
                  checked={!isArea}
                  onChange={() => setCaptureMode("display")}
                />
                Full screen
              </label>
            </fieldset>
          </div>

          <div className="vfl-record-stage">
            <div className="vfl-record-empty">
              <svg className="vfl-record-illustration" viewBox="0 0 200 152" fill="none" aria-hidden="true">
                <rect x="24" y="18" width="152" height="100" rx="8" />
                <path d="M24 100h152M100 118v18m-27 0h54" />
                {isArea ? (
                  <>
                    <rect className="vfl-record-area-outline" x="53" y="38" width="94" height="48" rx="2" />
                    <path className="vfl-record-area-corners" d="M48 44V33h12m80 0h12v11M48 80v11h12m80 0h12V80" />
                  </>
                ) : <rect className="vfl-record-display-outline" x="33" y="27" width="134" height="64" rx="3" />}
              </svg>
              <h3>{isArea ? "No area selected" : "No display selected"}</h3>
              <p>{isArea ? "Record a selected region of your screen." : "Record one full display."}</p>
              <button type="button" disabled aria-describedby="vfl-record-source-hint">
                {isArea ? "Select area" : "Choose display"}
              </button>
              <p id="vfl-record-source-hint" className="vfl-record-source-hint">
                Screen selection will be available with recording.
              </p>
            </div>
          </div>
        </section>

        <aside className="vfl-rail" aria-label="Recording settings">
          <section className="vfl-card is-open" aria-labelledby="vfl-record-audio-title">
            <div className="vfl-record-card-head">
              <h2 id="vfl-record-audio-title" className="vfl-card-title">Audio</h2>
            </div>
            <div className="vfl-card-body">
              <label className="vfl-check vfl-check-card">
                <input type="checkbox" checked={systemAudio} onChange={(event) => setSystemAudio(event.currentTarget.checked)} />
                <span>System audio</span>
              </label>
              <label className="vfl-check vfl-check-card">
                <input type="checkbox" checked={microphone} onChange={(event) => setMicrophone(event.currentTarget.checked)} />
                <span>Microphone</span>
              </label>
              <div className="vfl-inline-hint">Include either source, both, or record without audio.</div>
            </div>
          </section>

          <section className="vfl-card is-open" aria-labelledby="vfl-record-video-title">
            <div className="vfl-record-card-head">
              <h2 id="vfl-record-video-title" className="vfl-card-title">Video</h2>
            </div>
            <div className="vfl-card-body">
              <div className="vfl-field">
                <label htmlFor="vfl-record-quality">Quality</label>
                <select id="vfl-record-quality" value={quality} onChange={(event) => setQuality(event.currentTarget.value)}>
                  <option value="standard">Standard</option>
                  <option value="high">High</option>
                </select>
              </div>
              <div className="vfl-row2">
                <div className="vfl-field">
                  <label htmlFor="vfl-record-frame-rate">Frame rate</label>
                  <select id="vfl-record-frame-rate" value={frameRate} onChange={(event) => setFrameRate(event.currentTarget.value)}>
                    {[24, 25, 30, 50, 60].map((fps) => <option key={fps} value={String(fps)}>{fps} fps</option>)}
                  </select>
                </div>
                <div className="vfl-field">
                  <label htmlFor="vfl-record-countdown">Countdown</label>
                  <select id="vfl-record-countdown" value={countdown} onChange={(event) => setCountdown(event.currentTarget.value)}>
                    <option value="0">None</option>
                    <option value="3">3 seconds</option>
                    <option value="5">5 seconds</option>
                  </select>
                </div>
              </div>
              <label className="vfl-check">
                <input type="checkbox" checked={showCursor} onChange={(event) => setShowCursor(event.currentTarget.checked)} />
                <span>Show cursor</span>
              </label>
            </div>
          </section>

          <section className="vfl-card is-open" aria-labelledby="vfl-record-save-title">
            <div className="vfl-record-card-head">
              <h2 id="vfl-record-save-title" className="vfl-card-title">Save</h2>
            </div>
            <div className="vfl-card-body">
              <div className="vfl-record-format">
                <span className="vfl-field-label">Format</span>
                <span>MP4</span>
              </div>
              <div className="vfl-field">
                <span className="vfl-field-label">Output folder</span>
                <button type="button" disabled aria-describedby="vfl-record-folder-hint">Choose folder</button>
                <div id="vfl-record-folder-hint" className="vfl-inline-hint">
                  Folder selection will be available with recording.
                </div>
              </div>
            </div>
          </section>
        </aside>
      </div>

      <footer className="vfl-footer vfl-record-footer">
        <div className="vfl-footer-main">
          <div className="vfl-record-footer-copy">
            <div className="vfl-record-footer-line">
              <span className="vfl-footer-kicker">Record</span>
              <span id="vfl-record-unavailable">Recording isn’t available yet</span>
            </div>
            <div className="vfl-inline-hint">Adjust your settings here. Nothing is being captured.</div>
          </div>
          <div className="vfl-footer-actions">
            <button type="button" className="primary vfl-record-start" disabled aria-describedby="vfl-record-unavailable">
              Start recording
            </button>
          </div>
        </div>
      </footer>
    </div>
  );
}
