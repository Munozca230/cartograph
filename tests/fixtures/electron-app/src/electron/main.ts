import { app, ipcMain, BrowserWindow } from "electron";
import { CHANNELS } from "./channels";
import { saveDoc, checkForUpdates } from "./handlers";

function createWindow(): void {
  new BrowserWindow({ width: 800 });
}

ipcMain.handle(CHANNELS.PING, () => "pong");
ipcMain.handle("doc:save", saveDoc);
ipcMain.on("app:reload", () => createWindow());
ipcMain.handleOnce(CHANNELS.UPDATE_CHECK, async () => {
  return checkForUpdates();
});

// A generic registration: the channel is a parameter, not a name.
export function register(channel: string, fn: () => void): void {
  ipcMain.handle(channel, fn);
}

app.whenReady().then(createWindow);
