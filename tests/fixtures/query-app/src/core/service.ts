import { Model } from "./model";
import { save } from "./repo";

export function run() {
  save(new Model());
}
