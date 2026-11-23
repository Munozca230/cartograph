import { Button } from '@acme/ui-kit';

export function App() {
  window.api.saveDoc({ title: 'a' });
  const api = (window as any).api;
  api.getUser(1);
  // The preload exposes no notExposed.
  api.notExposed();
  window.api.onProgress((p: number) => p);
  return Button('ok');
}
