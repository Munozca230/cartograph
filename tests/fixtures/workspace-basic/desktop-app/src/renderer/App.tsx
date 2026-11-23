export function App() {
  const api = (window as any).api;
  api.getUser(1);
  api.notExposed();
  return null;
}
