import { contextBridge, ipcRenderer } from 'electron';
import { IPC, Push } from '../shared/channels';

function getUser(id: number) {
  return ipcRenderer.invoke(IPC.USERS_GET, id);
}

function pick(name: string) {
  return name.toLowerCase();
}

const api = {
  saveDoc: (doc: unknown) => ipcRenderer.invoke(IPC.DOCS_SAVE, doc),
  getUser,
  onProgress(cb: (p: number) => void) {
    ipcRenderer.on(Push.PROGRESS, (_e, p: number) => cb(p));
  },
  // No handler serves this channel.
  broken: () => ipcRenderer.invoke('nope'),
  // A channel the parse cannot read: counted, never reported.
  dynamic: (name: string) => ipcRenderer.invoke(pick(name)),
};

contextBridge.exposeInMainWorld('api', api);
