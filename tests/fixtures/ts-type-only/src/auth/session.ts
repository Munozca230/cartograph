import { userId } from "../model/user";

export interface Session {
  user: string;
}

export const openSession = (name: string): Session => ({ user: userId(name) });
