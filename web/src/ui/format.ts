const NUMBER = new Intl.NumberFormat("es");

export function formatNumber(n: number): string {
  return NUMBER.format(n);
}

const THOUSANDS = new Intl.NumberFormat("es", { maximumFractionDigits: 1 });

/** In millions once it would say 1000 mil or more. */
const inMillions = (n: number) => Math.round(n / 100) >= 10_000;

/** Line counts as people say them: 840, 1,3 mil, 191,5 mil, 1 millón, 1,5 millones. */
export function formatLoc(n: number): string {
  if (inMillions(n)) {
    const m = THOUSANDS.format(n / 1e6);
    return `${m} ${m === "1" ? "millón" : "millones"}`;
  }
  return n >= 1000 ? `${THOUSANDS.format(n / 1000)} mil` : NUMBER.format(n);
}

/** The unit after formatLoc: "1,5 millones de líneas", "1,3 mil líneas". */
export function locUnit(n: number, unit = "líneas"): string {
  return inMillions(n) ? `de ${unit}` : unit;
}

/**
 * Lines of code: the languages cartograph parses. The backend counts JSON, SQL, Markdown,
 * lockfiles and other data or markup under "other", so leaving it out is honest.
 */
export function codeLoc(languages: readonly { lang: string; loc: number }[]): number {
  return languages.filter((l) => l.lang !== "other").reduce((s, l) => s + l.loc, 0);
}

/** "1 par" / "3 pares". */
export function pairs(n: number): string {
  return `${NUMBER.format(n)} ${n === 1 ? "par" : "pares"}`;
}

const LANG_LABELS: Record<string, string> = {
  typescript: "TypeScript",
  tsx: "TSX",
  javascript: "JavaScript",
  python: "Python",
  rust: "Rust",
  go: "Go",
  java: "Java",
  c: "C",
  cpp: "C++",
  csharp: "C#",
  gdscript: "GDScript",
  other: "Otros",
};

export function langLabel(lang: string): string {
  return LANG_LABELS[lang] ?? lang;
}

export function describeKind(kind: string, lang?: string): string {
  if (kind === "file") return lang ? `Archivo ${langLabel(lang)}` : "Archivo";
  if (kind === "repo") return "Repositorio";
  if (kind === "dir") return "Carpeta";
  if (kind === "ext") return "Paquete externo";
  if (kind === "pkg") return "Manifiesto";
  return kind;
}

const REASONS: Record<string, string> = {
  not_found: "No existe el archivo",
  dynamic: "Import dinámico",
  alias_unresolved: "Alias sin configurar",
  ambiguous: "Ambiguo",
  too_large: "Demasiado grande",
  minified: "Minificado",
  parse_error: "Error de análisis",
  binary: "Binario",
  unsupported: "Lenguaje sin soporte",
};

export function reasonLabel(reason: string): string {
  return REASONS[reason] ?? reason.replaceAll("_", " ");
}

export function relativeTime(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `hace ${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `hace ${m} min`;
  const h = Math.round(m / 60);
  return `hace ${h} h`;
}

/** Display name of an id: last path segment, or the package name. */
export function nameOf(id: string): string {
  if (id.startsWith("ext:")) {
    const rest = id.slice(4);
    return rest.slice(rest.indexOf(":") + 1);
  }
  const path = id.slice(id.indexOf(":") + 1);
  if (!path) return "raíz";
  return path.slice(path.lastIndexOf("/") + 1);
}

export function pathOf(id: string): string {
  return id.slice(id.indexOf(":") + 1);
}

/** The `via` of an edge from an interface method to a Spring bean's method (the server's `SPRING_VIA`). */
export const SPRING_VIA = "Spring bean";

/** The label drawn on a dashed edge: a bridge's channel or route, or "bean de Spring". */
export function viaLabel(via: string): string {
  if (via === SPRING_VIA) return "bean de Spring";
  return via.length > 36 ? `${via.slice(0, 35)}…` : via;
}

/** The tooltip line of a dashed edge. */
export function viaTip(via: string): string {
  if (via === SPRING_VIA) return "Bean de Spring: la llamada a la interfaz ejecuta este método";
  return `Puente, no una llamada: ${via}`;
}
