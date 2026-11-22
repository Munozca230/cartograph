import { logCall } from "./log";

export async function saveDoc(_event: unknown, doc: string): Promise<number> {
  logCall("saveDoc");
  return doc.length;
}

export async function checkForUpdates(): Promise<boolean> {
  return false;
}
