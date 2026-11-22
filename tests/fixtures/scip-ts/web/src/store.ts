export class Store {
  items: string[] = [];
  save(x: string): void {
    this.items.push(x);
  }
  loadAll(): string[] {
    return this.items;
  }
}

export function makeStore(): Store {
  return new Store();
}

export const api = {
  fetch(): string[] {
    return makeStore().loadAll();
  },
};
