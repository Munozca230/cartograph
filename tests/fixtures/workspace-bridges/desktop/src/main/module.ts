import { ipcMain } from 'electron';

// Every module registers its handlers through `handle`: the channel is a parameter here,
// a literal or a constant at each call.
export abstract class Module {
  protected handle(channel: string, handler: (...args: unknown[]) => unknown) {
    ipcMain.handle(channel, async (_event, ...args: unknown[]) => handler(...args));
  }
}
