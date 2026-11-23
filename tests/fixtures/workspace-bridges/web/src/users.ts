import { http } from './http';

export function loadUser(id: string) {
  return http.get(`/users/${id}`);
}

// A wrapper: its parameter is the path.
export function getJson(path: string) {
  return http.get(path);
}

export function searchUsers() {
  return getJson('/users/search?q=a');
}

// The route is GET: the wrong method.
export function renameMe() {
  return http.put('/users/me', { name: 'x' });
}

export function runJob(id: number) {
  return fetch('http://localhost:3000/v1/jobs/' + id, { method: 'POST' });
}
