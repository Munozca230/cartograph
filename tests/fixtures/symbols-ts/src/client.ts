import axios from "axios";

// Not a route: an HTTP client call with a path and a callback.
export function load(cb: (x: unknown) => void): void {
  axios.get("/users/1", cb);
}
