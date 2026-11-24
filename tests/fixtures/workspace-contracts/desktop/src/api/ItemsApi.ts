import { ApiClient } from './ApiClient';
import type { BaseInput, TagInput } from './types';

/** What the client posts and patches: one type for the create and the update routes. */
interface ItemPayload extends BaseInput {
  label?: string;
  notes?: string;
  kind?: string;
  colour?: string;
  tags?: string[];
}

interface NoteInput {
  text: string;
  extra: string;
}

// Declaration merging: a second declaration adds to the first; the shape is not read.
interface NoteInput {
  mood?: string;
}

interface RenameInput {
  ref: string;
}

declare function pickMethod(): string;

// A class: its shape is not read.
class NoteDraft {
  text = '';
}

export class ItemsApi {
  constructor(private readonly client: ApiClient) {}

  private toPayload(label: string, tags: string[] | undefined): ItemPayload {
    const payload: ItemPayload = { label };
    if (tags) payload.tags = tags;
    return payload;
  }

  async create(label: string): Promise<void> {
    const payload = this.toPayload(label, undefined);
    await this.client.post<void>('/api/items', payload);
  }

  async update(id: string, label: string, tags: string[]): Promise<void> {
    const payload = this.toPayload(label, tags);
    await this.client.patch<void>(`/api/items/${id}`, payload);
  }

  async createMany(items: ItemPayload[]): Promise<void> {
    await this.client.post<void>('/api/items/batch', items);
  }

  // The route does not validate its body: nothing is required.
  async note(note: NoteInput): Promise<void> {
    await this.client.put<void>('/api/items/notes', note);
  }

  async draft(draft: NoteDraft): Promise<void> {
    await this.client.put<void>('/api/items/notes', draft);
  }

  // Leaves out `name`, which the route requires.
  async tag(tag: Pick<TagInput, 'hex'>): Promise<void> {
    await this.client.post<void>('/api/items/tags', tag);
  }

  async external(label: string): Promise<void> {
    await this.client.post<void>('/api/items/external', { label } as ItemPayload);
  }

  // A local typed with an object type written in place.
  async noteText(text: string): Promise<void> {
    const body: { text: string } = { text };
    await this.client.put<void>('/api/items/notes', body);
  }

  async rename(input: RenameInput): Promise<void> {
    await this.client.post<void>('/api/items/rename', input);
  }

  // A method known only at run time: a heuristic bridge, whose bodies are not kept.
  async guess(input: RenameInput): Promise<void> {
    const method = pickMethod();
    await this.client.request<void>(method, '/api/items/external', input);
  }

  // An object literal: a body of no known type.
  async raw(text: string): Promise<void> {
    await this.client.post<void>('/api/items/raw', { text });
  }
}
