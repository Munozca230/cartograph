// The plan's toolbar: how much of the tree is open (Nivel 1 · 2 · 3 · Todo), the layers that
// are off by default (libraries, unconnected cards, light edges), what this view says about
// cycles, the camera, and the legend.
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover";
import { LEVELS, type Level } from "../model/view";
import { cappedTodo, controller } from "../state/controller";
import { usePlan } from "../state/plan";
import { useAppState } from "../state/store";
import { formatNumber } from "./format";
import { Icon } from "./icons";
import { Legend } from "./Legend";

const LEVEL_HELP: Record<string, string> = {
  "1": "Nivel 1: solo las carpetas de primer nivel (cierra todo)",
  "2": "Nivel 2: abre las carpetas de primer nivel",
  "3": "Nivel 3: abre también las de segundo nivel",
  all: "Todo: el mapa completo, hasta los archivos",
};

function Levels() {
  const level = useAppState((s) => s.view.level);
  // The level "Todo" stopped at on a repo too big to open whole: Todo stays lit.
  const capped = useAppState((s) => cappedTodo(s.view.level, s.todoCap));
  // Pressing a lit Todo again keeps the folders opened by hand, so only closed ones count.
  const custom = useAppState((s) => s.view.closed.length + (cappedTodo(s.view.level, s.todoCap) === null ? s.view.open.length : 0) > 0);
  const active: Level = level === "all" || capped !== null ? "all" : level;
  return (
    <div className="levels" role="group" aria-label="Nivel de apertura">
      <span className="levels-label" aria-hidden="true">
        Nivel
      </span>
      {LEVELS.map((l) => (
        <button
          key={l}
          type="button"
          className="level"
          aria-pressed={active === l}
          data-custom={active === l && custom ? "" : undefined}
          title={active === l && custom ? `${LEVEL_HELP[String(l)]}. Pulsa para quitar lo que abriste o cerraste a mano.` : LEVEL_HELP[String(l)]}
          onClick={() => controller.setLevel(l)}
        >
          {l === "all" ? "Todo" : l}
        </button>
      ))}
      {capped !== null && (
        <span className="faint small" title="El repositorio es demasiado grande para abrirlo entero: se abre hasta este nivel, más las carpetas que ya tenías abiertas. Las que quedan cerradas se abren con doble clic.">
          hasta el nivel {capped}
        </span>
      )}
    </div>
  );
}

function Toggle({ on, onClick, icon, children, title }: { on: boolean; onClick: () => void; icon: "pkg" | "loose" | "eye" | "bridge"; children: React.ReactNode; title: string }) {
  return (
    <button type="button" className="chip toggle" aria-pressed={on} onClick={onClick} title={title}>
      <Icon name={icon} size={13} />
      {children}
    </button>
  );
}

export function Toolbar() {
  const plan = usePlan();
  const loading = useAppState((s) => s.graphLoading);
  const showExternals = useAppState((s) => s.showExternals);
  const showIsolated = useAppState((s) => s.showIsolated);
  const showAll = useAppState((s) => s.showAllEdges);
  const hasExternals = useAppState((s) => (s.graph?.externals.length ?? 0) > 0);
  const showBridges = useAppState((s) => s.showBridges);
  const bridges = useAppState((s) => s.graph?.bridges?.length ?? 0);
  const up = plan ? plan.allEdges.filter((e) => e.upstream).length : 0;
  const isolated = plan?.isolatedCount ?? 0;
  const hidden = plan?.hidden ?? 0;
  const lightOnly = plan?.lightOnly.length ?? 0;
  return (
    <div className="toolbar">
      <Levels />
      <span className="sep" aria-hidden="true" />
      {hasExternals && (
        <Toggle
          on={showExternals}
          icon="pkg"
          onClick={() => controller.setShowExternals(!showExternals)}
          title="Dibuja las librerías externas en la capa de abajo, agrupadas por ecosistema, con flechas desde lo que las usa"
        >
          Librerías
        </Toggle>
      )}
      {bridges > 0 && (
        <Toggle
          on={showBridges}
          icon="bridge"
          onClick={() => controller.setShowBridges(!showBridges)}
          title="Puentes: enlaces que ningún import declara (un canal IPC hasta su handler, una función que expone el preload, una ruta HTTP). Se dibujan punteados, encima del plano, sin moverlo."
        >
          Puentes
        </Toggle>
      )}
      {isolated > 0 && (
        <Toggle
          on={showIsolated}
          icon="loose"
          onClick={() => controller.setShowIsolated(!showIsolated)}
          title="Carpetas y archivos que no importan ni son importados por nada de esta vista. Pulsa para dibujarlos atenuados."
        >
          {formatNumber(isolated)} sin conexiones
        </Toggle>
      )}
      {(hidden > 0 || showAll) && (
        <Toggle
          on={showAll}
          icon="eye"
          onClick={() => controller.setShowAllEdges(!showAll)}
          title={`La vista es densa: no se dibujan las conexiones más livianas de cada caja (las que van contra la corriente se dibujan siempre).${lightOnly > 0 ? ` ${formatNumber(lightOnly)} ${lightOnly === 1 ? "elemento solo tiene" : "elementos solo tienen"} esas conexiones y vuelven con ellas.` : ""}`}
        >
          {showAll ? "Todas las conexiones" : `${formatNumber(hidden)} ${hidden === 1 ? "conexión oculta" : "conexiones ocultas"}`}
        </Toggle>
      )}
      {up > 0 && (
        <span className="chip warn" title="Dependencias que cierran un ciclo entre lo que se ve. Van en magenta, hacia arriba: es el sentido más liviano de cada ciclo.">
          <span className="pdot" aria-hidden="true" />
          {formatNumber(up)} contra la corriente
        </span>
      )}
      {loading && (
        <span className="faint" role="status">
          Abriendo…
        </span>
      )}
      <div className="toolbar-end">
        <div className="zoom">
          <button type="button" className="icon-btn" aria-label="Alejar" title="Alejar (−)" onClick={() => controller.zoom(1 / 1.25)}>
            <Icon name="minus" />
          </button>
          <button type="button" className="icon-btn" aria-label="Acercar" title="Acercar (+)" onClick={() => controller.zoom(1.25)}>
            <Icon name="plus" />
          </button>
          <button type="button" className="btn fit" title="Encuadrar el plano (0)" onClick={() => controller.fitAll()}>
            <Icon name="fit" />
            <span className="fit-text">Encuadrar</span>
          </button>
        </div>
        <Popover>
          <PopoverTrigger className="icon-btn" aria-label="Cómo leer el plano" title="Cómo leer el plano">
            <Icon name="help" />
          </PopoverTrigger>
          <PopoverContent className="pop legend-pop" side="bottom" align="end" sideOffset={8}>
            <PopoverTitle className="pop-title">Cómo leer el plano</PopoverTitle>
            <Legend />
          </PopoverContent>
        </Popover>
      </div>
    </div>
  );
}
