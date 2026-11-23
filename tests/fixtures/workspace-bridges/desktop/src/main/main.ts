import { app, BrowserWindow, ipcMain } from 'electron';
import { IPC, Push } from '../shared/channels';
import { DocsModule } from './docs';
import { getUser } from './users';

function channelFor(name: string) {
  return `plugin:${name}`;
}

app.whenReady().then(() => {
  const win = new BrowserWindow();
  new DocsModule().register();
  ipcMain.handle(IPC.USERS_GET, getUser);
  // A channel built at run time: counted, never matched.
  ipcMain.handle(channelFor(app.getName()), () => null);
  win.webContents.send(Push.PROGRESS, 0);
});
