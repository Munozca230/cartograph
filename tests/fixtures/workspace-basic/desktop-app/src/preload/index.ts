import { contextBridge, ipcRenderer } from 'electron';
import { IPC } from '../shared/channels';

function getUser(id: number) {
  return ipcRenderer.invoke(IPC.USERS_GET, id);
}

function onProgress(cb: (p: number) => void) {
  ipcRenderer.on('progress', (_e, p: number) => cb(p));
}

contextBridge.exposeInMainWorld('api', {
  getUser,
  onProgress,
  broken: () => ipcRenderer.invoke('nope'),
});
