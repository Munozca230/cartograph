export class Store {
  save(): void {
    this.flushPending();
  }

  flushPending(): void {}

  compactSegments(): number {
    return 0;
  }
}

export default Store;
