import { contextBridge, ipcRenderer } from 'electron';

const shopApi = {
  loadOrder: (id: string) => ipcRenderer.invoke('order:load', id),
  saveOrder: (order: unknown) => ipcRenderer.invoke('order:save', order),
};

contextBridge.exposeInMainWorld('shopApi', shopApi);
