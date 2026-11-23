export class ApiClient {
  constructor(private base: string) {}

  async request(method: string, path: string, body?: unknown): Promise<unknown> {
    const res = await fetch(this.base + path, { method, body: JSON.stringify(body) });
    return res.json();
  }

  get(path: string) {
    return this.request('GET', path);
  }

  post(path: string, body: unknown) {
    return this.request('POST', path, body);
  }
}
