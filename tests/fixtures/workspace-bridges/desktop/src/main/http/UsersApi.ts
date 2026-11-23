import { ApiClient } from './ApiClient';

export interface User {
  id: number;
}

export class UsersApi {
  constructor(private readonly client: ApiClient) {}

  async getUser(id: number): Promise<User> {
    return await this.client.get<User>(`/api/users/${id}`);
  }

  async me(): Promise<User> {
    return this.client.get<User>('/api/users/me');
  }

  async create(user: User): Promise<User> {
    return this.client.post<User>('/api/users', user);
  }

  // No member serves this route: an unmatched call.
  async report(): Promise<void> {
    await this.client.post<void>('/api/reports/generate', {});
  }
}

// A URL only known at run time: counted, never matched or reported.
export async function download(next: () => string): Promise<Response> {
  return fetch(next());
}
