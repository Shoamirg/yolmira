import { useStore } from "../store";

/** Shown whenever the daemon can't be reached. The UI reconnects by itself. */
export function Banner() {
  const daemon = useStore((s) => s.daemon);
  const startDaemon = useStore((s) => s.startDaemon);
  if (daemon.connected) return null;
  return (
    <div className="banner" role="alert" data-testid="daemon-banner">
      <span className="banner-dot" aria-hidden="true" />
      <div className="banner-text">
        <strong>Daemon unreachable.</strong> Shell states are unknown until it's back; reconnecting every second.
        {daemon.error && <span className="banner-why"> {daemon.error.why}</span>}
      </div>
      <button type="button" className="btn btn-quiet" onClick={() => void startDaemon()} data-testid="daemon-start">
        Start daemon
      </button>
    </div>
  );
}
