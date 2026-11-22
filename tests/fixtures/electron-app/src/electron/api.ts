import { logCall } from "./log";

export const api = {
  start: () => logCall("start"),
  stop() {
    logCall("stop");
  },
};

const helpers = {
  twice: (n: number) => n * 2,
};

export default {
  name: "plugin",
  run() {
    api.start();
    return helpers.twice(2);
  },
};
