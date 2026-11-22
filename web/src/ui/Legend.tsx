// "Cómo leer el plano": the content of the toolbar's "?" popover. First what each card is
// (kinds, model/kind.ts), then how cards and connections read.
import { KIND_LABEL, KINDS, type Kind } from "../model/kind";
import { KindGlyph } from "./kindIcons";

const KIND_HINT: Record<Kind, string> = {
  code: "ts, py, rs, go, java…",
  test: "test, spec, __tests__…",
  script: "scripts/, sh, ps1…",
  style: "css, scss, less",
  data: "json, yaml, md…",
};

/** A small card: kind stripe and glyph, plus a tab for a folder. */
function KindSwatch({ kind, folder = false }: { kind: Kind; folder?: boolean }) {
  return (
    <svg width="34" height="20" aria-hidden="true" className={`k-${kind}`}>
      <rect className="lg-card" x="1.5" y="4.5" width="31" height="14" rx="3" />
      {folder && <path className="lg-tab" d="M5 4.9 6.8 1.8h9.4L18 4.9" />}
      <path className="lg-stripe" d="M3.2 4.9A3 3 0 0 0 1.5 7.5v8a3 3 0 0 0 1.7 2.6Z" />
      <KindGlyph kind={kind} folder={folder} className="lg-glyph" transform="translate(8 5.5) scale(0.75)" />
    </svg>
  );
}

export function Legend() {
  return (
    <>
      <p className="legend-h">Qué es cada cosa</p>
      <ul className="legend">
        <li>
          <KindSwatch kind="code" folder />
          <span>Carpeta, del color de lo que más tiene dentro</span>
        </li>
        {KINDS.map((k) => (
          <li key={k}>
            <KindSwatch kind={k} />
            <span>
              {KIND_LABEL[k]} <span className="legend-hint">{KIND_HINT[k]}</span>
            </span>
          </li>
        ))}
      </ul>
      <p className="legend-h">Cómo se lee</p>
      <ul className="legend">
        <li>
          <svg width="34" height="18" aria-hidden="true">
            <rect className="lg-card" x="1.5" y="1.5" width="31" height="15" rx="3" />
            <rect className="lg-bar" x="9" y="12" width="16" height="2" rx="1" />
          </svg>
          <span>La barra es su tamaño en líneas</span>
        </li>
        <li>
          <svg width="34" height="18" aria-hidden="true">
            <rect className="lg-card" x="4" y="4" width="28" height="12.5" rx="3" />
            <rect className="lg-card" x="1.5" y="1.5" width="28" height="12.5" rx="3" />
          </svg>
          <span>Carpeta cerrada: doble clic la abre</span>
        </li>
        <li>
          <svg width="34" height="18" aria-hidden="true">
            <rect className="lg-box" x="1.5" y="1.5" width="31" height="15" rx="4" />
          </svg>
          <span>Carpeta abierta: doble clic en su nombre la cierra</span>
        </li>
        <li>
          <svg width="34" height="12" aria-hidden="true">
            <path className="lg-route" d="M2 6h24" strokeWidth="1.6" />
            <path className="lg-route-head" d="M25 2.5l5 3.5-5 3.5z" />
          </svg>
          <span>Depende de (hacia abajo); más gruesa, más archivos</span>
        </li>
        <li>
          <svg width="34" height="12" aria-hidden="true">
            <path className="lg-alert" d="M2 6h24" strokeWidth="1.8" />
            <path className="lg-alert-head" d="M25 2.5l5 3.5-5 3.5z" />
          </svg>
          <span>Contra la corriente: cierra un ciclo</span>
        </li>
        <li>
          <svg width="34" height="12" aria-hidden="true">
            <circle className="lg-dot" cx="17" cy="6" r="4" />
          </svg>
          <span>Tiene problemas o está en un ciclo</span>
        </li>
        <li>
          <svg width="34" height="12" aria-hidden="true">
            <path className="lg-route" d="M2 6h30" strokeWidth="1.6" strokeDasharray="3 3" />
          </svg>
          <span>Import resuelto de forma aproximada</span>
        </li>
        <li>
          <svg width="34" height="18" aria-hidden="true">
            <rect className="lg-ext" x="1.5" y="2.5" width="31" height="13" rx="6.5" />
          </svg>
          <span>Librería externa (botón Librerías)</span>
        </li>
      </ul>
    </>
  );
}
