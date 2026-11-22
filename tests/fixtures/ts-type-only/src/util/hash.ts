// Re-exports a type from auth/: type-only, so util/ does not depend on auth/ at load time.
export type { Token } from "../auth/token";

export const hash = (s: string) => s.length.toString(16);
