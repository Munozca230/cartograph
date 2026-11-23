import { Module } from './module';
import { IPC } from '../shared/channels';

export class DocsModule extends Module {
  register() {
    this.handle(IPC.DOCS_SAVE, this.onSave.bind(this));
    this.handle('docs:unused', this.onUnused.bind(this));
  }

  onSave(doc: unknown) {
    return doc;
  }

  onUnused() {
    return null;
  }
}
