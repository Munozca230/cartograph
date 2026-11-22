import plugin, { api } from "../electron/api";

export function boot(): void {
  api.stop();
  plugin.run();
}
