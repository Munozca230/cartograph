// A client whose base URL is known only at run time, three wrappers deep, the body
// passed on through an options object: post/patch/put -> request -> send -> fetch.
export interface RequestOptions {
  timeout?: number;
}

export class ApiClient {
  constructor(private readonly baseUrl: string) {}

  private async send(url: string, options: RequestOptions | undefined, init: RequestInit): Promise<Response> {
    const controller = new AbortController();
    return fetch(url, { ...init, signal: controller.signal });
  }

  async request<T>(method: string, endpoint: string, body?: unknown, options?: RequestOptions): Promise<T> {
    const url = `${this.baseUrl}${endpoint}`;
    const send = (bearer?: string) =>
      this.send(url, options, {
        method,
        headers: { Authorization: `Bearer ${bearer}` },
        body: body ? JSON.stringify(body) : undefined,
      });
    const res = await send();
    return (await res.json()) as T;
  }

  get<T>(endpoint: string): Promise<T> {
    return this.request<T>('GET', endpoint);
  }

  post<T>(endpoint: string, body: unknown, options?: RequestOptions): Promise<T> {
    return this.request<T>('POST', endpoint, body, options);
  }

  put<T>(endpoint: string, body: unknown): Promise<T> {
    return this.request<T>('PUT', endpoint, body);
  }

  patch<T>(endpoint: string, body: unknown): Promise<T> {
    return this.request<T>('PATCH', endpoint, body);
  }
}
