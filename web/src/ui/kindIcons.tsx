// Line glyphs for what a card is (model/kind.ts), on a 16-unit grid. They stroke with
// currentColor, so the card or the legend colors them with the kind's token.
import type { Kind } from "../model/kind";

const GLYPHS: Record<Kind | "folder", React.ReactNode> = {
  folder: <path d="M1.8 4.2a1 1 0 0 1 1-1h3.4l1.6 1.7h5.4a1 1 0 0 1 1 1v6.9a1 1 0 0 1-1 1H2.8a1 1 0 0 1-1-1Z" />,
  // </>: source code.
  code: <path d="M5.6 4.2 2 8l3.6 3.8M10.4 4.2 14 8l-3.6 3.8" />,
  // A flask: tests.
  test: <path d="M6.2 2h3.6M6.8 2v4.4L2.9 12.6a.9.9 0 0 0 .8 1.4h8.6a.9.9 0 0 0 .8-1.4L9.2 6.4V2M4.6 10h6.8" />,
  // A prompt: scripts.
  script: <path d="M2.5 4.5 6.5 8l-4 3.5M8.5 12.5h5" />,
  // A drop of paint: styles.
  style: <path d="M8 2.2s4.6 4.7 4.6 7.6a4.6 4.6 0 0 1-9.2 0C3.4 6.9 8 2.2 8 2.2Z" />,
  // A page with lines: config, data and docs.
  data: <path d="M4 1.8h5.4L12.5 5v9.2H4ZM9.2 1.8v3.4h3.3M6.2 8.4h4.2M6.2 11h4.2" />,
};

/** The glyph of a kind, or the folder glyph; place and scale it with `transform`. */
export function KindGlyph({ kind, folder = false, transform, className }: { kind: Kind; folder?: boolean; transform?: string; className?: string }) {
  return (
    <g className={className} transform={transform} fill="none" stroke="currentColor" strokeWidth={1.5} strokeLinecap="round" strokeLinejoin="round">
      {GLYPHS[folder ? "folder" : kind]}
    </g>
  );
}
