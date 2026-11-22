// Every specifier is `type`, but not `import type`: under `verbatimModuleSyntax` this
// becomes `import {} from "./session"` and loads it, so it is a runtime dependency. The
// other imports name a type inline.
import { type Session, type Session as S } from "./session";

export type Token = { session: Session; copy: S; owner: import("../model/user").User };
export type Store = typeof import("../util/hash");
