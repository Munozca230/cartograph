import { ipcMain, BrowserWindow } from 'electron';
import { IPC } from '../shared/channels';
import { getUser } from './users';

export function registerIpc(win: BrowserWindow) {
  ipcMain.handle(IPC.USERS_GET, (_e, id: number) => getUser(id));
  ipcMain.on('log', (_e, line: string) => console.log(line));
  win.webContents.send('progress', 0);
}
