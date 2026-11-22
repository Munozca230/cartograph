// Uses the session only for its type: no load-time dependency on auth/.
import type { Session } from "../auth/session";
import { hash } from "../util/hash";

export interface User {
  name: string;
  session?: Session;
}

export const userId = (name: string) => hash(name);
