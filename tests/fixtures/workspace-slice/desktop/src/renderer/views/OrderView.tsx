export function OrderView() {
  const load = (id: string) => window.shopApi.loadOrder(id);
  const save = (order: unknown) => window.shopApi.saveOrder(order);
  return { load, save };
}
