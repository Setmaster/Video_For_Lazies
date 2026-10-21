import type { RecordingAudioDevice, RecordingCapabilities } from "../lib/recording";
import type { AudioApplication, AudioLevels, AudioMeter, RecordingAudioConfig } from "../lib/recordingAudioTypes";
import type { useRecordingAudio } from "../lib/recordingAudio";

type Props = { config: RecordingAudioConfig; setConfig: (value: RecordingAudioConfig) => void;
  devices: RecordingAudioDevice[]; applications: AudioApplication[]; truncated: boolean;
  capabilities: RecordingCapabilities | null; locked: boolean; recording: boolean; mixer: ReturnType<typeof useRecordingAudio> };
const meterText = (meter?: AudioMeter) => !meter ? "Levels available while recording" : ({ off: "Off", pending: "Waiting for audio", active: "Active", silent: "Silent", muted: "Muted · Input still active", failed: "Input unavailable" })[meter.state];
function Meter({ name, meter }: { name: string; meter?: AudioMeter }) {
  return <div className="vfl-audio-meter-row" id={`vfl-record-meter-${name.toLowerCase().replace(/ /g, "-")}`} data-meter-state={meter?.state || "off"} data-meter-peak={meter?.peak ?? 0} data-meter-rms={meter?.rms ?? 0} data-meter-overload={meter?.overload || false}><meter min={0} max={1} value={Math.min(1, meter?.peak || 0)} aria-label={`${name} peak level`} />
    <span className={meter?.overload ? "vfl-error" : "vfl-inline-hint"}>{meter?.overload ? "Clipping" : meterText(meter)}</span></div>;
}
export default function RecordingAudioControls({ config, setConfig, devices, applications, truncated, capabilities, locked, recording, mixer }: Props) {
  const systems = devices.filter(device => device.kind === "system"), microphones = devices.filter(device => device.kind === "microphone");
  const levels = recording ? mixer.desired : config;
  const changeLevel = (source: "renderLevel" | "microphoneLevel", patch: Partial<AudioLevels["renderLevel"]>) => {
    const next = { renderLevel: levels.renderLevel, microphoneLevel: levels.microphoneLevel, [source]: { ...levels[source], ...patch } };
    if (recording) mixer.change(next); else setConfig({ ...config, ...next });
  };
  const levelRow = (source: "renderLevel" | "microphoneLevel", name: string, meter?: AudioMeter) => <div className="vfl-audio-source-controls">
    <div className="vfl-audio-level-label"><label htmlFor={`vfl-record-${source}-gain`}>{name} volume</label><output>{Math.round(levels[source].gain * 100)}%</output></div>
    <input id={`vfl-record-${source}-gain`} type="range" min="0" max="200" step="1" value={Math.round(levels[source].gain * 100)}
      disabled={recording ? !mixer.ready : locked} aria-valuetext={`${Math.round(levels[source].gain * 100)} percent`}
      onChange={event => changeLevel(source, { gain: Number(event.currentTarget.value) / 100 })} />
    <label className="vfl-check"><input id={`vfl-record-${source}-mute`} type="checkbox" checked={levels[source].muted} disabled={recording ? !mixer.ready : locked}
      onChange={event => changeLevel(source, { muted: event.currentTarget.checked })} /><span>Mute {name.toLowerCase()}</span></label>
    <Meter name={name} meter={recording ? meter : undefined} />
  </div>;
  return <section className="vfl-card is-open" aria-labelledby="vfl-record-audio-title">
    <div className="vfl-record-card-head"><h2 id="vfl-record-audio-title" className="vfl-card-title">Audio</h2></div>
    <div className="vfl-card-body" id="vfl-record-audio" data-audio-session={mixer.status?.sessionId || ""} data-audio-generation={mixer.status?.sourceGeneration || ""}
      data-audio-accepted-sequence={mixer.status?.acceptedCommandSequence ?? 0} data-audio-rejected-sequence={mixer.status?.rejectedCommandSequence ?? ""} data-audio-applied-sequence={mixer.status?.appliedCommandSequence ?? 0} data-audio-applied-sample={mixer.status?.appliedAtSample || ""} data-audio-pending={mixer.pending}>
      <div className="vfl-field"><label htmlFor="vfl-record-render-source">Audio source</label><select id="vfl-record-render-source" disabled={locked} value={config.render.kind}
        onChange={event => setConfig({ ...config, render: event.currentTarget.value === "system" ? { kind: "system", deviceId: systems[0]?.id || "" } : event.currentTarget.value === "application" ? { kind: "application", applicationId: "" } : { kind: "off" } })}>
        <option value="off">Off</option><option value="system">System output</option><option value="application">Application</option></select></div>
      {config.render.kind === "system" && <div className="vfl-field"><label htmlFor="vfl-record-system-device">System audio device</label><select id="vfl-record-system-device" value={config.render.deviceId} disabled={locked}
        onChange={event => setConfig({ ...config, render: { kind: "system", deviceId: event.currentTarget.value } })}>
        {!systems.some(device => device.id === (config.render.kind === "system" ? config.render.deviceId : "")) && <option value={config.render.deviceId}>Selected device unavailable</option>}
        {systems.map(device => <option key={device.id} value={device.id}>{device.name}</option>)}</select></div>}
      {config.render.kind === "application" && <><div className="vfl-field"><label htmlFor="vfl-record-audio-application">Audio application</label><select id="vfl-record-audio-application" value={config.render.applicationId} disabled={locked || !capabilities?.audio.application.available}
        onChange={event => setConfig({ ...config, render: { kind: "application", applicationId: event.currentTarget.value } })}>
        <option value="">Choose an application</option>{config.render.applicationId && !applications.some(app => app.id === (config.render.kind === "application" ? config.render.applicationId : "")) && <option value={config.render.applicationId}>Selected application unavailable</option>}
        {applications.map(app => <option key={app.id} value={app.id}>{app.name} · {app.processName}{app.available ? "" : " · Unavailable"}</option>)}</select></div>
        <div className="vfl-inline-hint">{!capabilities?.audio.application.available ? capabilities?.audio.application.reason || "Application audio is unavailable." : "Captures the chosen process and its children. A browser may include several tabs. Video and audio targets are independent."}{truncated ? " Application list limited; close unused apps and refresh if needed." : ""}</div>
        {applications.find(app => app.id === (config.render.kind === "application" ? config.render.applicationId : ""))?.reason && <div className="vfl-error">{applications.find(app => app.id === (config.render.kind === "application" ? config.render.applicationId : ""))?.reason}</div>}</>}
      {config.render.kind !== "off" && levelRow("renderLevel", config.render.kind === "application" ? "Application" : "System", mixer.status?.render)}
      <label className="vfl-check vfl-check-card"><input type="checkbox" checked={config.microphone !== null} disabled={locked || (!config.microphone && microphones.length === 0)}
        onChange={event => setConfig({ ...config, microphone: event.currentTarget.checked ? { deviceId: microphones[0]?.id || "", suppression: "off" } : null })} /><span>Microphone</span></label>
      {config.microphone && <><div className="vfl-field"><label htmlFor="vfl-record-microphone-device">Microphone device</label><select id="vfl-record-microphone-device" value={config.microphone.deviceId} disabled={locked}
        onChange={event => setConfig({ ...config, microphone: { ...config.microphone!, deviceId: event.currentTarget.value } })}>
        {!microphones.some(device => device.id === config.microphone?.deviceId) && <option value={config.microphone.deviceId}>Selected device unavailable</option>}
        {microphones.map(device => <option key={device.id} value={device.id}>{device.name}</option>)}</select></div>
        {levelRow("microphoneLevel", "Microphone", mixer.status?.microphone)}
        <label className="vfl-check"><input id="vfl-record-noise-reduction" type="checkbox" checked={config.microphone.suppression === "save"} disabled={locked || !capabilities?.audio.microphoneSuppression.available}
          onChange={event => setConfig({ ...config, microphone: { ...config.microphone!, suppression: event.currentTarget.checked ? "save" : "off" } })} /><span>Noise reduction</span></label>
        <div className="vfl-inline-hint">Applied when saving. Live levels are measured before noise reduction.{!capabilities?.audio.microphoneSuppression.available ? ` ${capabilities?.audio.microphoneSuppression.reason || "Noise reduction is unavailable."}` : ""}</div></>}
      {(config.render.kind !== "off" || config.microphone) && <><Meter name="Mixed output" meter={recording ? mixer.status?.mixed : undefined} /><div className="vfl-inline-hint">Mixed playback plus separate selected-source tracks.</div></>}
      <div className="vfl-inline-hint">Microphone is off by default. Audio starts only when you record.</div>
      {recording && (config.render.kind !== "off" || config.microphone) && <div className="vfl-inline-hint" role="status">{mixer.status?.rejectedCommandSequence != null ? "Audio change rejected; last confirmed levels remain in effect." : mixer.pending ? "Applying audio changes…" : mixer.status ? "Levels confirmed by the recorder." : "Waiting for recorder audio status."}</div>}
      {mixer.error && <div className="vfl-error" role="alert">{mixer.error} <button type="button" onClick={() => void mixer.retry()}>Retry audio status</button></div>}
    </div>
  </section>;
}
