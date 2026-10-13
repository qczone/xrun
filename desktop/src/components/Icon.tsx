export type IconName =
  | "monitor"
  | "devices"
  | "settings"
  | "arrow"
  | "shield"
  | "refresh"
  | "folder"
  | "terminal"
  | "activity"
  | "lock"
  | "more"
  | "play"
  | "pause"
  | "plus"
  | "search"
  | "copy"
  | "check"
  | "chevron-down"
  | "close"
  | "image"
  | "up"
  | "down"
  | "power"
  | "transfer"
  | "camera"
  | "chevron-right"
  | "network";

export function Icon({
  name,
  className = "",
}: {
  name: IconName;
  className?: string;
}) {
  return (
    <svg className={`icon ${className}`} aria-hidden="true">
      <use href={`#i-${name}`} />
    </svg>
  );
}

export function IconDefinitions() {
  return (
    <svg className="icon-defs" aria-hidden="true">
      <defs>
        <symbol id="i-chevron-down" viewBox="0 0 24 24">
          <path d="m6 9 6 6 6-6" />
        </symbol>
        <symbol id="i-chevron-right" viewBox="0 0 24 24">
          <path d="m9 6 6 6-6 6" />
        </symbol>
        <symbol id="i-network" viewBox="0 0 24 24">
          <circle cx="12" cy="5" r="2.5" />
          <circle cx="5" cy="18" r="2.5" />
          <circle cx="19" cy="18" r="2.5" />
          <path d="M10.8 7.2 6.3 15.8m6.9-8.6 4.5 8.6M7.5 18h9" />
        </symbol>
        <symbol id="i-power" viewBox="0 0 24 24">
          <path d="M12 3v8" />
          <path d="M7.05 6.3a8 8 0 1 0 9.9 0" />
        </symbol>
        <symbol id="i-transfer" viewBox="0 0 24 24">
          <path d="M8 20V5M4 9l4-4 4 4M16 4v15m4-4-4 4-4-4" />
        </symbol>
        <symbol id="i-camera" viewBox="0 0 24 24">
          <path d="M4 8a2 2 0 0 1 2-2h2l1.5-2h5L16 6h2a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z" />
          <circle cx="12" cy="12.5" r="3.5" />
        </symbol>
        <symbol id="i-check" viewBox="0 0 24 24">
          <path d="m5 12 4 4L19 6" />
        </symbol>
        <symbol id="i-copy" viewBox="0 0 24 24">
          <rect x="8" y="8" width="12" height="12" rx="2" />
          <path d="M16 8V4H4v12h4" />
        </symbol>
        <symbol id="i-activity" viewBox="0 0 24 24">
          <path d="M3 12h4l3-7 4 14 3-7h4" />
        </symbol>
        <symbol id="i-lock" viewBox="0 0 24 24">
          <rect x="5" y="10" width="14" height="11" rx="2" />
          <path d="M8 10V7a4 4 0 0 1 8 0v3m-4 5v2" />
        </symbol>
        <symbol id="i-more" viewBox="0 0 24 24">
          <circle cx="5" cy="12" r=".7" />
          <circle cx="12" cy="12" r=".7" />
          <circle cx="19" cy="12" r=".7" />
        </symbol>
        <symbol id="i-play" viewBox="0 0 24 24">
          <path d="m8 4 12 8-12 8z" />
        </symbol>
        <symbol id="i-pause" viewBox="0 0 24 24">
          <path d="M9 5v14M15 5v14" />
        </symbol>
        <symbol id="i-plus" viewBox="0 0 24 24">
          <path d="M12 5v14M5 12h14" />
        </symbol>
        <symbol id="i-search" viewBox="0 0 24 24">
          <circle cx="10.5" cy="10.5" r="6.5" />
          <path d="m16 16 4 4" />
        </symbol>
        <symbol id="i-close" viewBox="0 0 24 24">
          <path d="m6 6 12 12M6 18 18 6" />
        </symbol>
        <symbol id="i-image" viewBox="0 0 24 24">
          <rect x="3" y="3" width="18" height="18" rx="2" />
          <circle cx="8" cy="8" r="1.5" />
          <path d="m3 17 5-5 4 4 4-6 5 7" />
        </symbol>
        <symbol id="i-up" viewBox="0 0 24 24">
          <path d="M12 20V4M6 10l6-6 6 6" />
        </symbol>
        <symbol id="i-down" viewBox="0 0 24 24">
          <path d="M12 4v16m-6-6 6 6 6-6" />
        </symbol>
        <symbol id="i-monitor" viewBox="0 0 24 24">
          <rect x="3" y="4" width="18" height="13" rx="2" />
          <path d="M8 21h8m-4-4v4" />
        </symbol>
        <symbol id="i-devices" viewBox="0 0 24 24">
          <rect x="2" y="4" width="13" height="11" rx="2" />
          <path d="M5 19h7m-3-4v4" />
          <rect x="17" y="9" width="5" height="11" rx="1.5" />
        </symbol>
        <symbol id="i-settings" viewBox="0 0 24 24">
          <path d="M4 7h16M4 17h16" />
          <circle cx="9" cy="7" r="3" />
          <circle cx="16" cy="17" r="3" />
        </symbol>
        <symbol id="i-arrow" viewBox="0 0 24 24">
          <path d="M5 12h14m-6-6 6 6-6 6" />
        </symbol>
        <symbol id="i-shield" viewBox="0 0 24 24">
          <path d="m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z" />
          <path d="m8 12 3 3 5-6" />
        </symbol>
        <symbol id="i-refresh" viewBox="0 0 24 24">
          <path d="M20 11a8 8 0 1 0-2 6M20 4v7h-7" />
        </symbol>
        <symbol id="i-folder" viewBox="0 0 24 24">
          <path d="M3 7V5a2 2 0 0 1 2-2h5l2 3h7a2 2 0 0 1 2 2v11H3z" />
        </symbol>
        <symbol id="i-terminal" viewBox="0 0 24 24">
          <rect x="3" y="4" width="18" height="16" rx="2" />
          <path d="m7 9 3 3-3 3m6 0h4" />
        </symbol>
      </defs>
    </svg>
  );
}
