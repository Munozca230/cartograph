import { find } from "./catalog";

export function runRead(path: string): string {
  const entry = find("READ-FILE");
  return `${entry?.label ?? "?"} ${path}`;
}
