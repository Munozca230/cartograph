// One value import and one type import from the same file: the dependency is a runtime one.
import { openSession, type Session } from "./auth/session";
import type { Token } from "./auth/token";
// A re-export whose every name is `type` still loads the module: a runtime dependency.
export { type Token as HashToken } from "./util/hash";

const s: Session = openSession("ada");
const t: Token | null = null;
console.log(s, t, await import("./model/user"));
