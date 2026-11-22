import { Store } from "./store";

export function show(cfg: { loadAll(): string[] }, st: Store): number {
  st.save("x");
  return cfg.loadAll().length;
}
