import { ipcMain } from 'electron';
import { OrderClient } from '../api/OrderClient';

export class OrderModule {
  private client = new OrderClient();

  register() {
    ipcMain.handle('order:load', this.onLoad.bind(this));
    ipcMain.handle('order:save', async (_e, order: unknown) => {
      const saved = await this.onSave(order);
      return saved;
    });
    ipcMain.handle('order:ping', () => this.client.loadOrder('ping'));
  }

  onLoad(_e: unknown, id: string) {
    return this.client.loadOrder(id);
  }

  onSave(order: unknown) {
    return this.client.saveOrder(order);
  }
}
