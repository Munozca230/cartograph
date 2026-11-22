// The slim header: explorer button, project name with a live dot, problems (only when there
// are some), search, and an info button whose popover holds the numbers and the theme.
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@ui-sdk/react/tooltip";
import { useEffect, useMemo, useState } from "react";
import { problemCount } from "../model/findings";
import { controller } from "../state/controller";
import { flows } from "../state/flows";
import { store, useAppState } from "../state/store";
import { CommandPalette } from "./CommandPalette";
import { codeLoc, formatLoc, formatNumber, locUnit, relativeTime } from "./format";
import { Icon, Mark } from "./icons";
import { ThemeToggle } from "./ThemeToggle";

export function TopBar() {
  const name = useAppState((s) => s.meta?.repo.name ?? "");
  const root = useAppState((s) => s.meta?.repo.root ?? "");
  const explorerOpen = useAppState((s) => s.explorerOpen);
  return (
    <header className="top">
      <button
        type="button"
        className="icon-btn"
        aria-label="Explorador de carpetas"
        title="Explorador de carpetas"
        aria-expanded={explorerOpen}
        onClick={() => store.set({ explorerOpen: !explorerOpen })}
      >
        <Icon name="tree" />
      </button>
      <div className="brand">
        <Mark />
        <span className="brand-app">cartograph</span>
        {name && (
          <h1 className="proj" title={root}>
            {name}
          </h1>
        )}
        <LiveDot />
        <ViewSwitch />
      </div>
      <div className="top-right">
        <IssuesButton />
        <CommandPalette />
        <InfoButton />
      </div>
    </header>
  );
}

/** Plano | Flujos: two views of the same index, each with its own camera and selection. */
function ViewSwitch() {
  const mode = useAppState((s) => s.mode);
  return (
    <div className="views" role="group" aria-label="Vista">
      <button type="button" className="view-tab" aria-pressed={mode === "plan"} title="Plano: carpetas y archivos, y quién importa a quién" onClick={() => flows.setMode("plan")}>
        Plano
      </button>
      <button type="button" className="view-tab" aria-pressed={mode === "flows"} title="Flujos: qué funciones llama cada entrada (main, rutas, comandos)" onClick={() => flows.setMode("flows")}>
        Flujos
      </button>
    </div>
  );
}

/** A dot next to the name: live, indexing (with its percentage), idle or offline. Click: re-index. */
function LiveDot() {
  const watching = useAppState((s) => s.meta?.watching ?? false);
  const phase = useAppState((s) => s.phase);
  const reindexing = useAppState((s) => s.reindexing);
  const done = useAppState((s) => s.progress?.done ?? null);
  const total = useAppState((s) => s.progress?.total ?? null);
  const last = useAppState((s) => s.lastIndexedAt);
  const connection = useAppState((s) => s.connection);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), 5000);
    return () => window.clearInterval(t);
  }, []);

  let state: "live" | "indexing" | "idle" | "offline";
  let text: string;
  const pct = done !== null && total ? Math.round((done / total) * 100) : null;
  if (phase === "indexing" || reindexing) {
    state = "indexing";
    text = pct !== null ? `Indexando ${pct} %` : "Indexando";
  } else if (connection === "reconnecting" || phase === "error") {
    state = "offline";
    text = "Reconectando";
  } else if (!watching) {
    state = "idle";
    text = "Sin vigilancia";
  } else {
    state = "live";
    text = "En vivo";
  }
  const updated = last ? ` Actualizado ${relativeTime(now - last)}.` : "";
  const description =
    state === "live"
      ? `En vivo: cartograph vigila el repositorio y redibuja el plano cuando cambia un archivo.${updated} Pulsa para reindexar.`
      : state === "idle"
        ? `Sin vigilancia: el servidor no vigila los archivos.${updated} Pulsa para reindexar a mano.`
        : state === "indexing"
          ? "Se está leyendo el repositorio. El plano se actualiza al terminar."
          : "Se perdió la conexión con el servidor. Se reintenta solo.";

  return (
    <Tooltip delay={300}>
      <TooltipTrigger className="live" data-state={state} onClick={() => void controller.reindex()} aria-label={`${text}. Reindexar ahora`}>
        <span key={last ?? 0} className="dot" aria-hidden="true" />
        {/* Only the transient states say something; "live" is the quiet dot. */}
        {(state === "indexing" || state === "offline") && <span aria-hidden="true">{text}</span>}
      </TooltipTrigger>
      <TooltipContent className="tooltip" side="bottom" sideOffset={8}>
        {description}
      </TooltipContent>
    </Tooltip>
  );
}

function IssuesButton() {
  const unresolved = useAppState((s) => s.issues?.total.unresolved ?? s.meta?.counts?.unresolved ?? 0);
  const skipped = useAppState((s) => s.issues?.total.skipped ?? s.meta?.counts?.skipped ?? 0);
  const diagnostics = useAppState((s) => s.issues?.diagnostics.length ?? s.meta?.counts?.diagnostics ?? 0);
  // Findings that are problems (errors and warnings) in the whole repo, from the summary:
  // the list itself may be cut at the request's limit. Informational ones are not counted.
  const findings = useAppState((s) => (s.findings ? problemCount(s.findings) : (s.meta?.counts?.findings ?? 0)));
  const total = unresolved + skipped + diagnostics + findings;
  if (total === 0) return null;
  const label = `${formatNumber(total)} ${total === 1 ? "problema" : "problemas"}`;
  return (
    <button
      type="button"
      className="chip issues"
      aria-label={`${label}: hallazgos de arquitectura, imports sin resolver, diagnósticos y omitidos`}
      title="Hallazgos de arquitectura, imports sin resolver, diagnósticos y archivos omitidos"
      onClick={() => store.set({ issuesOpen: true })}
      aria-haspopup="dialog"
    >
      <span className="pdot" aria-hidden="true" />
      <span aria-hidden="true">{label}</span>
    </button>
  );
}

/** ⓘ: the repository's numbers and the theme, out of the way. */
function InfoButton() {
  const files = useAppState((s) => s.meta?.counts?.files ?? null);
  const imports = useAppState((s) => s.meta?.counts?.imports ?? null);
  const languages = useAppState((s) => s.meta?.languages);
  const graph = useAppState((s) => s.graph);
  const name = useAppState((s) => s.meta?.repo.name ?? "");
  const root = useAppState((s) => s.meta?.repo.root ?? "");
  const loc = useMemo(() => (languages ? codeLoc(languages) : null), [languages]);
  const packages = useMemo(() => (graph ? new Set(graph.externals.map((x) => x.ext)).size : null), [graph]);
  const rows: [string, string, string?][] = [];
  if (files !== null) rows.push([formatNumber(files), "archivos"]);
  if (loc !== null && loc > 0) rows.push([formatLoc(loc), locUnit(loc, "líneas de código"), "Líneas de los lenguajes que cartograph analiza; no cuenta JSON, SQL, Markdown ni lockfiles"]);
  if (imports !== null) rows.push([formatNumber(imports), "imports"]);
  if (packages !== null) rows.push([formatNumber(packages), packages === 1 ? "librería externa" : "librerías externas"]);
  return (
    <Popover>
      <PopoverTrigger className="icon-btn" aria-label="Información del repositorio y tema" title="Información y tema">
        <Icon name="info" />
      </PopoverTrigger>
      <PopoverContent className="pop info-pop" side="bottom" align="end" sideOffset={8}>
        <PopoverTitle className="pop-title">{name}</PopoverTitle>
        {root && <p className="pop-sub">{root}</p>}
        <dl className="info-stats">
          {rows.map(([v, label, title]) => (
            <div key={label} title={title}>
              <dt>{v}</dt>
              <dd>{label}</dd>
            </div>
          ))}
        </dl>
        <div className="pop-row">
          <span>Tema</span>
          <ThemeToggle />
        </div>
      </PopoverContent>
    </Popover>
  );
}
