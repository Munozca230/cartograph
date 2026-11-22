export interface Named {
  name(): string;
}

export interface Drawable extends Named {
  draw(): void;
}

export abstract class Shape implements Drawable {
  static create(): void {}

  describe(): string {
    return this.name();
  }

  protected scale(f: number): number {
    return f;
  }

  abstract name(): string;
  abstract draw(): void;
}
