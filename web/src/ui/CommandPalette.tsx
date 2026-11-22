import { useAsyncOptions } from "@ui-sdk/react/async-options";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandGroupLabel,
  CommandInput,
  CommandItem,
  CommandList,
  CommandStatus,
} from "@ui-sdk/react/command";
import { Dialog, DialogBackdrop, DialogContent, DialogShortcut, DialogTitle, DialogTrigger } from "@ui-sdk/react/dialog";
import { useMemo, useState } from "react";
import { createSearch, groupResults, matchRanges, type PaletteResult } from "../api/search";
import type { FunctionResult } from "../api/types";
import { controller } from "../state/controller";
import { flows } from "../state/flows";
import { store, useAppState } from "../state/store";
import { formatNumber } from "./format";
import { Icon } from "./icons";

interface Action {
  value: string;
  label: string;
  hint: string;
  run: () => void;
}

const ACTIONS: Action[] = [
  { value: "action:top", label: "Cerrar todo (nivel 1)", hint: "Solo las carpetas de primer nivel", run: () => controller.setLevel(1) },
  { value: "action:all", label: "Abrir todo", hint: "El mapa completo, hasta los archivos", run: () => controller.setLevel("all") },
  { value: "action:fit", label: "Encuadrar el plano", hint: "Todo el plano a la vista", run: () => controller.fitAll() },
  { value: "action:ext", label: "Mostrar u ocultar las librerías", hint: "Paquetes externos en la capa de abajo", run: () => controller.setShowExternals(!store.get().showExternals) },
  { value: "action:explorer", label: "Abrir el explorador", hint: "El árbol de carpetas", run: () => store.set({ explorerOpen: true }) },
  { value: "action:light", label: "Mostrar u ocultar las conexiones livianas", hint: "Solo en vistas densas", run: () => controller.setShowAllEdges(!store.get().showAllEdges) },
  { value: "action:issues", label: "Ver problemas", hint: "Imports sin resolver, diagnósticos y omitidos", run: () => store.set({ issuesOpen: true }) },
  { value: "action:reindex", label: "Reindexar ahora", hint: "Vuelve a leer el repositorio", run: () => void controller.reindex() },
];

function Highlight({ text, ranges }: { text: string; ranges: [number, number][] }) {
  if (ranges.length === 0) return <>{text}</>;
  const out: React.ReactNode[] = [];
  let at = 0;
  ranges.forEach(([a, b], i) => {
    if (a > at) out.push(text.slice(at, a));
    out.push(<mark key={i}>{text.slice(a, b)}</mark>);
    at = b;
  });
  if (at < text.length) out.push(text.slice(at));
  return <>{out}</>;
}

function KindGlyph({ kind }: { kind: PaletteResult["kind"] | "action" }) {
  if (kind === "dir")
    return (
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M2.5 8.2 5.3 3.4h5.4l2.8 4.8-2.8 4.8H5.3Z" />
      </svg>
    );
  if (kind === "file")
    return (
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <circle cx="8" cy="8" r="3.2" />
      </svg>
    );
  if (kind === "ext")
    return (
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M8 2.6 13.4 8 8 13.4 2.6 8Z" />
      </svg>
    );
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M3 8h10M9.5 4.5 13 8l-3.5 3.5" />
    </svg>
  );
}

export function CommandPalette() {
  const open = useAppState((s) => s.paletteOpen);
  const [query, setQuery] = useState("");
  const search = useMemo(() => createSearch(controller.api, 40), []);
  const results = useAsyncOptions<PaletteResult>(search, { debounce: 140 });
  // Functions open in Flujos; an index without symbols just finds none.
  const findFunctions = useMemo(
    () =>
      async (query: string, { signal }: { signal: AbortSignal }): Promise<FunctionResult[]> =>
        query.trim().length < 2 ? [] : (await flows.api.searchFunctions(query.trim(), 8, signal)).results,
    [],
  );
  const functions = useAsyncOptions<FunctionResult>(findFunctions, { debounce: 140 });
  const groups = groupResults(results.items);
  const q = query.trim().toLowerCase();
  const actions = q ? ACTIONS.filter((a) => a.label.toLowerCase().includes(q)) : ACTIONS;

  const run = (value: string) => {
    store.set({ paletteOpen: false });
    const action = ACTIONS.find((a) => a.value === value);
    if (action) action.run();
    else if (value.startsWith("fn:")) void flows.open(value.slice(3));
    else controller.select(value, { fly: true });
  };

  const regions = useAppState((s) => s.graph);
  const suggestions = !q && regions
    ? [...regions.nodes].filter((n) => n.kind === "dir").sort((a, b) => b.fan_in - a.fan_in).slice(0, 6)
    : [];

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        store.set({ paletteOpen: next });
        if (!next) {
          setQuery("");
          results.setQuery("");
          functions.setQuery("");
        }
      }}
    >
      <DialogTrigger className="search" shortcut="mod+k" aria-label="Buscar">
        <Icon name="search" />
        <span className="search-text">Buscar</span>
        <DialogShortcut className="kbd" />
      </DialogTrigger>
      <DialogBackdrop className="palette-backdrop" />
      <DialogContent className="palette" initialFocus="input">
        <DialogTitle className="visually-hidden">Buscar en el plano</DialogTitle>
        <Command
          filter={false}
          status={q ? results.status : "idle"}
          query={query}
          onQueryChange={(next) => {
            setQuery(next);
            results.setQuery(next);
            functions.setQuery(next);
          }}
          onAction={(value) => run(value)}
          loop
        >
          <div className="palette__field">
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="7" cy="7" r="4.3" />
              <path d="m10.2 10.2 3.3 3.3" />
            </svg>
            <CommandInput className="palette__input" aria-label="Buscar carpetas, archivos o paquetes" placeholder="Buscar carpetas, archivos o paquetes…" />
          </div>
          <CommandStatus className="visually-hidden">
            {(state: { loading: boolean; count: number }) =>
              q ? (state.loading ? "Buscando" : `${state.count} resultados`) : ""
            }
          </CommandStatus>
          <CommandEmpty className="palette__empty">
            {(state: { empty: boolean; loading: boolean; query: string }) =>
              state.empty && !state.loading && q ? `Nada coincide con “${state.query}”. Prueba con parte de una ruta, como “db/schema”.` : null
            }
          </CommandEmpty>
          <CommandList className="palette__list">
            {suggestions.length > 0 && (
              <CommandGroup className="palette__group">
                <CommandGroupLabel className="palette__group-label">Lo más usado en esta vista</CommandGroupLabel>
                {suggestions.map((n) => (
                  <CommandItem key={n.id} value={n.id} label={n.name} className="palette__item">
                    <span className="palette__glyph" data-kind="dir">
                      <KindGlyph kind="dir" />
                    </span>
                    <span className="palette__label">{n.name}</span>
                    <span className="palette__context">
                      {n.path}, {formatNumber(n.fan_in)} entrantes
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {groups.map((g) => (
              <CommandGroup key={g.group} className="palette__group">
                <CommandGroupLabel className="palette__group-label">{g.group}</CommandGroupLabel>
                {g.items.map((r) => (
                  <CommandItem key={r.value} value={r.value} label={r.label} className="palette__item">
                    <span className="palette__glyph" data-kind={r.kind}>
                      <KindGlyph kind={r.kind} />
                    </span>
                    <span className="palette__label">
                      <Highlight text={r.label} ranges={r.matches} />
                    </span>
                    <span className="palette__context">{r.context}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
            {q && functions.items.length > 0 && (
              <CommandGroup className="palette__group">
                <CommandGroupLabel className="palette__group-label">Funciones (en Flujos)</CommandGroupLabel>
                {functions.items.map((f) => (
                  <CommandItem key={f.id} value={`fn:${f.id}`} label={f.name} className="palette__item">
                    <span className="palette__glyph" data-kind="action">
                      <KindGlyph kind="action" />
                    </span>
                    <span className="palette__label">
                      <Highlight text={f.name} ranges={matchRanges(f.name, q)} />
                    </span>
                    <span className="palette__context">
                      {f.path}:{f.line}
                    </span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
            {actions.length > 0 && (
              <CommandGroup className="palette__group">
                <CommandGroupLabel className="palette__group-label">Acciones</CommandGroupLabel>
                {actions.map((a) => (
                  <CommandItem key={a.value} value={a.value} label={a.label} className="palette__item">
                    <span className="palette__glyph" data-kind="action">
                      <KindGlyph kind="action" />
                    </span>
                    <span className="palette__label">{a.label}</span>
                    <span className="palette__context">{a.hint}</span>
                  </CommandItem>
                ))}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
        <footer className="palette__footer" aria-hidden="true">
          <span>
            <kbd>↑</kbd> <kbd>↓</kbd> elegir
          </span>
          <span>
            <kbd>Intro</kbd> ir
          </span>
          <span>
            <kbd>Esc</kbd> cerrar
          </span>
        </footer>
      </DialogContent>
    </Dialog>
  );
}
