import { ipcMain } from "electron";
import { checkForUpdates } from "./handlers";

export class Updater {
  register(): void {
    ipcMain.handle("update:now", async () => checkForUpdates());
  }
}
