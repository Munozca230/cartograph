import { makeStore, api } from "./index";
import { format } from "./util";
import { debounce } from "lodash";

export function run(input: string): void {
  const s = makeStore();
  s.save(format(input));
  helper();
  debounce(run, 10);
  api.fetch();
}

function helper(): void {
  console.log("hi");
}

export function read(cfg: { load(): string[] }): string[] {
  return cfg.load();
}
