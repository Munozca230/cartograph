import { Ring } from './circle';
import { Mid } from './mid';

export class App {
  private ring: Ring = new Ring();
  private mid: Mid = new Mid();

  run(): void {
    this.ring.describe();
    this.mid.rootHelper();
    this.mid.missingEverywhere();
    Ring.create();
  }
}
