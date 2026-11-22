import { run } from "../core/service";
import { connect } from "../infra/db";
import { widget } from "./widgets";

export function start() {
  connect();
  widget();
  run();
}
