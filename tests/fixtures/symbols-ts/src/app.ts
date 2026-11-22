import { slugify, DataStore } from "./index";
import Store from "./store";
import * as u from "./util";
import { dup } from "./both";
import * as a from "./dup-a";
import { get } from "lodash";

export function run(title: string): string {
  const store = new DataStore();
  store.compactSegments();
  store.save();
  const other = new Store();
  other.flushPending();
  u.helper();
  a.dup();
  dup();
  get({}, "x");
  missing();
  return slugify(title);
}
