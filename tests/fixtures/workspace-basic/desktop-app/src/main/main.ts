import { app, BrowserWindow } from 'electron';
import { registerIpc } from './ipc';

app.whenReady().then(() => {
  const win = new BrowserWindow();
  registerIpc(win);
});
