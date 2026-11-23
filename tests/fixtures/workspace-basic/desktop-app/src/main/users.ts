import { ApiClient } from './apiClient';

const api = new ApiClient('http://localhost:8080/api');

export function getUser(id: number) {
  return api.get(`/users/${id}`);
}

export function getMe() {
  return api.get('/users/me');
}

export function createUser(name: string) {
  return api.request('POST', '/users', { name });
}

export function renameUser() {
  return api.request('PATCH', '/users/1', { name: 'x' });
}
