// Small line icons. They draw with currentColor, so both themes color them from CSS.
const PATHS = {
  search: (
    <>
      <circle cx="7" cy="7" r="4.8" />
      <path d="M10.6 10.6 14 14" />
    </>
  ),
  minus: <path d="M4 8h8" />,
  plus: <path d="M4 8h8M8 4v8" />,
  fit: <path d="M2.5 5.5V2.5h3M10.5 2.5h3v3M13.5 10.5v3h-3M5.5 13.5h-3v-3" />,
  close: <path d="m4 4 8 8M12 4l-8 8" />,
  sun: (
    <>
      <circle cx="8" cy="8" r="2.9" />
      <path d="M8 1.8v1.5M8 12.7v1.5M1.8 8h1.5M12.7 8h1.5M3.6 3.6l1.1 1.1M11.3 11.3l1.1 1.1M3.6 12.4l1.1-1.1M11.3 4.7l1.1-1.1" />
    </>
  ),
  moon: <path d="M12.5 10.1A5.3 5.3 0 0 1 5.9 3.5a5.3 5.3 0 1 0 6.6 6.6Z" />,
  tree: (
    <>
      <rect x="2" y="2.5" width="12" height="11" rx="1.8" />
      <path d="M6 2.5v11" />
    </>
  ),
  info: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 7.2v3.8M8 4.9v.1" />
    </>
  ),
  help: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M6.3 6.3a1.8 1.8 0 1 1 2.5 1.7c-.5.2-.8.6-.8 1.1v.4M8 11.3v.1" />
    </>
  ),
  bridge: <path d="M1.5 11.5h13M3.5 11.5V7.5M12.5 11.5V7.5M1.5 7.5c2.5 0 3.5-3 6.5-3s4 3 6.5 3" />,
  pkg: (
    <>
      <path d="M8 1.8 13.6 5v6L8 14.2 2.4 11V5Z" />
      <path d="M2.4 5 8 8.2 13.6 5M8 8.2v6" />
    </>
  ),
  loose: (
    <>
      <rect x="2" y="3" width="5" height="4" rx="1" />
      <rect x="9" y="9" width="5" height="4" rx="1" />
    </>
  ),
  eye: (
    <>
      <path d="M1.8 8s2.3-4.2 6.2-4.2S14.2 8 14.2 8s-2.3 4.2-6.2 4.2S1.8 8 1.8 8Z" />
      <circle cx="8" cy="8" r="1.8" />
    </>
  ),
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 14 }: { name: IconName; size?: number }) {
  return (
    <svg className="icon" width={size} height={size} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {PATHS[name]}
    </svg>
  );
}

/** The cartograph mark: a compass rose in brass. */
export function Mark() {
  return (
    <svg className="mark" width="20" height="20" viewBox="0 0 20 20" fill="none" aria-hidden="true">
      <circle cx="10" cy="10" r="8.4" />
      <path d="M10 2.5v15M2.5 10h15" opacity=".45" />
      <path className="needle" d="M10 5.2l1.6 4.8L10 14.8 8.4 10z" />
    </svg>
  );
}
