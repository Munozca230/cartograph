import axios from 'axios';

const API = 'http://localhost:8080';

// An axios instance: its baseURL goes before every path of its calls.
export const http = axios.create({ baseURL: API + '/api' });
