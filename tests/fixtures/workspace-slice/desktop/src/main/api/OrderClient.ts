import { http } from './http';

export class OrderClient {
  loadOrder(id: string) {
    return http.get(`/api/orders/${id}`);
  }

  saveOrder(order: unknown) {
    return http.post('/api/orders', order);
  }
}
