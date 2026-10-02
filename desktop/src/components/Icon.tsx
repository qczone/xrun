export type IconName =
  | "monitor"
  | "devices"
  | "settings"
  | "arrow"
  | "shield"
  | "refresh"
  | "folder"
  | "terminal";

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
