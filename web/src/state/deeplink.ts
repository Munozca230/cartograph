// Applies links into the map (see model/deeplink.ts): the one the page was opened with,
// once the first plan is loaded, and the one of each history step (popstate). Then it keeps
// the URL in step with the selection (replaceState), so the address bar can be shared.
// A link is applied once: later re-indexes and reconnections never move the camera again.
import { toast } from "@ui-sdk/react/toast";
import { currentLink, hasLink, linkSearch, linkView, parseDeepLink, planTarget, sameLink, type DeepLink } from "../model/deeplink";
import { controller as planController, type Controller } from "./controller";
import { flows as flowsController, type FlowsController } from "./flows";
import { store } from "./store";

/** The parts of `window` the links use (a fake in tests). */
export interface LinkWindow {
  location: { search: string; hash: string; pathname: string };
  history: { replaceState(data: unknown, unused: string, url?: string): void };
  addEventListener(type: "popstate", l: () => void): void;
}

export interface LinkDeps {
  plan: Pick<Controller, "focus">;
  flows: Pick<FlowsController, "open" | "setMode" | "loadEntries">;
  /** Shows a short notice that doesn't block the page. */
  notify: (title: string, description: string) => void;
}

const defaultDeps = (): LinkDeps => ({
  plan: planController,
  flows: flowsController,
  notify: (title, description) => toast({ title, description, duration: 6000 }),
});

export class DeepLinks {
  constructor(
    private readonly win: LinkWindow,
    private readonly deps: LinkDeps = defaultDeps(),
  ) {}

  private started = false;
  private syncing = false;
  /** The link last written to (or read from) the URL. */
  private written: DeepLink | null = null;

  /** Reads the page's link, applies it once the plan is ready, then follows the selection. */
  start() {
    if (this.started) return;
    this.started = true;
    this.win.addEventListener("popstate", () => void this.apply(parseDeepLink(this.win.location.search)));
    const link = parseDeepLink(this.win.location.search);
    if (!hasLink(link)) {
      this.sync();
      return;
    }
    const whenReady = () => {
      if (store.get().phase !== "ready") return false;
      // After the load that made the plan ready has finished its own updates.
      void Promise.resolve()
        .then(() => this.apply(link))
        .finally(() => this.sync());
      return true;
    };
    if (whenReady()) return;
    const stop = store.subscribe(() => {
      if (whenReady()) stop();
    });
  }

  /** Shows what `link` names: the plan on a file or folder, or the flows at a function. */
  async apply(link: DeepLink) {
    this.written = link;
    const focus = link.focus;
    if (linkView(link) === "flows") {
      if (!focus) {
        this.deps.flows.setMode("flows");
        return;
      }
      // The entry list beside the flow (Flujos loads it on its first visit otherwise).
      if (store.get().flowEntries === null) void this.deps.flows.loadEntries();
      await this.deps.flows.open(focus);
      const s = store.get();
      if (s.flowFrom?.id === focus && s.flowError !== null) {
        this.deps.notify("No se encontró en Flujos", `${focus}: ${s.flowError}`);
        store.set({ flowFrom: null, flow: null, flowError: null, flowSelected: null, flowCard: null });
        void this.deps.flows.loadEntries({ openFirst: true });
      }
      return;
    }
    store.set({ mode: "plan" });
    if (!focus) return;
    const h = store.get().hierarchy;
    const id = h ? planTarget(h, focus) : null;
    if (!id) {
      this.deps.notify("No está en el plano", `${focus} no es un archivo ni una carpeta de este repositorio.`);
      return;
    }
    // The whole repo: the plan as it opens, nothing to select.
    if (id !== h?.root) this.deps.plan.focus(id);
  }

  /** From now on, the URL follows the view and its selection. */
  private sync() {
    if (this.syncing) return;
    this.syncing = true;
    const write = () => {
      const s = store.get();
      if (s.phase !== "ready") return;
      const link = currentLink(s);
      if (this.written && sameLink(link, this.written)) return;
      this.written = link;
      const { pathname, search, hash } = this.win.location;
      const next = linkSearch(search, link);
      if (next !== search) this.win.history.replaceState(null, "", `${pathname}${next}${hash}`);
    };
    store.subscribe(write);
    write();
  }
}
