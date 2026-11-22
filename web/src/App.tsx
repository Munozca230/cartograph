import { Toast, ToastClose, ToastDescription, ToastRegion, ToastTitle } from "@ui-sdk/react/toast";
import { useEffect } from "react";
import { controller } from "./state/controller";
import { DeepLinks } from "./state/deeplink";
import { store, useAppState } from "./state/store";
import { flows } from "./state/flows";
import { closeTopmost, DetailPanel } from "./ui/DetailPanel";
import { Explorer } from "./ui/Explorer";
import { FlowsView, FlowToolbar } from "./ui/FlowsView";
import { Icon, Mark } from "./ui/icons";
import { IssuesDrawer } from "./ui/IssuesDrawer";
import { PlanView } from "./ui/PlanView";
import { Toolbar } from "./ui/Toolbar";
import { TopBar } from "./ui/TopBar";

function BootOverlay() {
  const phase = useAppState((s) => s.phase);
  const error = useAppState((s) => s.bootError);
  const done = useAppState((s) => s.progress?.done ?? null);
  const total = useAppState((s) => s.progress?.total ?? null);
  const name = useAppState((s) => s.meta?.repo.name ?? null);
  if (phase === "ready") return null;
  const pct = done !== null && total ? Math.round((done / total) * 100) : null;
  return (
    <div className="boot" role="status" aria-live="polite">
      <div className="boot-card">
        <Mark />
        {phase === "error" ? (
          <>
            <h2>No se pudo trazar el plano</h2>
            <p>{error}</p>
            <button type="button" className="btn primary" onClick={() => void controller.start()}>
              Reintentar
            </button>
          </>
        ) : phase === "indexing" ? (
          <>
            <h2>Trazando el plano{name ? ` de ${name}` : ""}</h2>
            <p>{pct !== null ? `Leídos ${done} de ${total} archivos` : "Recorriendo el repositorio"}</p>
            <progress max={100} value={pct ?? undefined} aria-label="Progreso del indexado" />
          </>
        ) : (
          <>
            <h2>{phase === "loading" ? "Dibujando el plano" : "Conectando"}</h2>
            <progress aria-label="Cargando" />
          </>
        )}
      </div>
    </div>
  );
}

const links = new DeepLinks(window);

export function App() {
  const mode = useAppState((s) => s.mode);
  useEffect(() => {
    controller.boot();
    flows.watch();
    // `?view=…&focus=…`: applied once the first plan is loaded, then kept in step.
    links.start();
    // Profiling and test handle: in dev, and in builds with ?debug in the URL.
    if (import.meta.env.DEV || new URLSearchParams(location.search).has("debug")) {
      Object.assign(window, { __cartograph: { controller, store, flows } });
    }
    // Esc closes the topmost layer the app owns (dialogs and popovers close themselves first).
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      // A dialog, drawer or popover is open: it closes itself.
      if (document.querySelector("[role=dialog]")) return;
      if (closeTopmost()) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="app">
      <a className="skip-link" href="#plan">
        Saltar al plano
      </a>
      <TopBar />
      {mode === "plan" ? <Toolbar /> : <FlowToolbar />}
      {/* Both views stay mounted, so each keeps its camera; the hidden one keeps its size too. */}
      <div className="main">
        <main className="pane" id="plan" tabIndex={-1} data-hidden={mode === "plan" ? undefined : ""} inert={mode !== "plan"}>
          <PlanView />
        </main>
        {mode === "plan" && <DetailPanel />}
        <FlowsView active={mode === "flows"} />
        <BootOverlay />
      </div>
      <Explorer />
      <IssuesDrawer />
      <ToastRegion className="toasts" side="bottom" align="end">
        {() => (
          <Toast className="toast">
            <div className="toast-text">
              <ToastTitle className="toast-title" />
              <ToastDescription className="toast-description" />
            </div>
            <ToastClose className="icon-btn" aria-label="Cerrar aviso">
              <Icon name="close" />
            </ToastClose>
          </Toast>
        )}
      </ToastRegion>
    </div>
  );
}
