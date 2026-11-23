// A client whose base URL is known only at run time, three wrappers deep:
// get/post -> request -> send -> fetch.
export class ApiClient {
  constructor(private readonly baseUrl: string) {}

  private async send(url: string, init: RequestInit): Promise<Response> {
    const controller = new AbortController();
    return fetch(url, { ...init, signal: controller.signal });
  }

  async request<T>(method: string, endpoint: string, body?: unknown): Promise<T> {
    const url = `${this.baseUrl}${endpoint}`;
    const go = () => this.send(url, { method, body: JSON.stringify(body) });
    const res = await go();
    return (await res.json()) as T;
  }

  get<T>(endpoint: string): Promise<T> {
    return this.request<T>('GET', endpoint);
  }

  post<T>(endpoint: string, body: unknown): Promise<T> {
    return this.request<T>('POST', endpoint, body);
  }
}
