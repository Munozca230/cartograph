import { contextBridge, ipcRenderer } from "electron";
import { CHANNELS } from "./channels";
import { logCall } from "./log";

const deskApi = {
  version: "1.0",
  ping: () => ipcRenderer.invoke(CHANNELS.PING),
  async save(doc: string) {
    logCall("save");
    return ipcRenderer.invoke("doc:save", doc);
  },
  updater: {
    check: async () => {
      logCall("check");
      return ipcRenderer.invoke(CHANNELS.UPDATE_CHECK);
    },
  },
  "on-status": function (cb: (s: string) => void) {
    ipcRenderer.on("status", (_e, s) => cb(s));
  },
};

contextBridge.exposeInMainWorld("desk", deskApi);

contextBridge.exposeInMainWorld("tools", {
  format: (s: string) => logCall(s),
  reload() {
    return ipcRenderer.invoke("app:reload");
  },
});

function makeLocal() {
  // A local object: its functions are not symbols.
  const inner = { skip: () => logCall("inner") };
  return inner;
}

makeLocal();
