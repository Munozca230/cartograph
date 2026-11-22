import type { Plugin } from "vite";
import { MockServer, type MockOptions } from "./server.ts";

/** Vite plugin that answers /api/* from the mock fixtures in `vite` (dev) and `vite preview`. */
export function mockApi(opts: MockOptions): Plugin {
  let server: MockServer | null = null;
  return {
    name: "cartograph-mock-api",
    configureServer(vite) {
      server = new MockServer(opts);
      vite.config.logger.info(`  cartograph: API simulada con el fixture "${opts.fixture}"`);
      vite.middlewares.use((req, res, next) => {
        if (!server?.handle(req, res)) next();
      });
      vite.httpServer?.once("close", () => server?.dispose());
    },
    configurePreviewServer(preview) {
      const mock = new MockServer(opts);
      preview.config.logger.info(`  cartograph: API simulada con el fixture "${opts.fixture}"`);
      preview.middlewares.use((req, res, next) => {
        if (!mock.handle(req, res)) next();
      });
      preview.httpServer?.once("close", () => mock.dispose());
    },
  };
}
