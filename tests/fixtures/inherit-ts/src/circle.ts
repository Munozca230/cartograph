import { Shape } from './shape';

export class Circle extends Shape {
  name(): string {
    return 'circle';
  }

  draw(): void {
    this.describe();
    this.scale(2);
    super.describe();
  }
}

export class Ring extends Circle {
  draw(): void {
    super.draw();
    this.describe();
  }
}
