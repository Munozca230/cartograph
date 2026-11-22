import { Model } from "../core/model";
import { util } from "../core/util";

export function connect() {
  return [new Model(), util];
}
