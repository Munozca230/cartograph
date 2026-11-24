import { ApiClient } from './ApiClient';
import type { BaseInput, TagInput } from './types';

/** What the client posts and patches: one type for the create and the update routes. */
interface ItemPayload extends BaseInput {
  title?: string;
  summary?: string;
  kind?: string;
  colour?: string;
  tags?: string[];
}

interface NoteInput {
  text: string;
  extra: string;
}

export class ItemsApi {
  constructor(private readonly client: ApiClient) {}

  private toPayload(title: string, tags: string[] | undefined): ItemPayload {
    const payload: ItemPayload = { title };
    if (tags) payload.tags = tags;
    return payload;
  }

  async create(title: string): Promise<void> {
    const payload = this.toPayload(title, undefined);
    await this.client.post<void>('/api/items', payload);
  }

  async update(id: string, title: string, tags: string[]): Promise<void> {
    const payload = this.toPayload(title, tags);
    await this.client.patch<void>(`/api/items/${id}`, payload);
  }

  async createMany(items: ItemPayload[]): Promise<void> {
    await this.client.post<void>('/api/items/batch', items);
  }

  // The route does not validate its body: nothing is required.
  async note(note: NoteInput): Promise<void> {
    await this.client.put<void>('/api/items/notes', note);
  }

  // Leaves out `name`, which the route requires.
  async tag(tag: Pick<TagInput, 'hex'>): Promise<void> {
    await this.client.post<void>('/api/items/tags', tag);
  }

  async external(title: string): Promise<void> {
    await this.client.post<void>('/api/items/external', { title } as ItemPayload);
  }

  // An object literal: a body of no known type.
  async raw(text: string): Promise<void> {
    await this.client.post<void>('/api/items/raw', { text });
  }
}
