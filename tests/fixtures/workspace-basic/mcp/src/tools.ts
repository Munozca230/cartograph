import axios from 'axios';
import { IPC } from '../../desktop-app/src/shared/channels';

const BASE = process.env.API_BASE ?? 'http://localhost:8080/api';

export async function lookupUser(id: number) {
  const res = await axios.get(BASE + '/users/' + id);
  return { channel: IPC.USERS_GET, user: res.data };
}
