/** The document steps an agent may ask for, by tool name (exporting is not offered). */
export const STEP_TO_KEY = {
  open: "OPEN",
  close: "CLOSE",
  save: "SAVE",
  print: "PRINT",
  share: "SHARE",
  import: "IMPORT",
  archive: "ARCHIVE",
  restore: "RESTORE",
  lock: "LOCK",
  unlock: "UNLOCK",
} as const;

export function keyOf(tool: keyof typeof STEP_TO_KEY): string {
  return STEP_TO_KEY[tool];
}
