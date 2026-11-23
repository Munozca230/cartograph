import { Router } from 'express';

export const jobs = Router();

jobs.post('/jobs/:id', runJob);
jobs.get('/jobs/:id?', listJobs);

function runJob(_req: unknown, _res: unknown) {}

function listJobs(_req: unknown, _res: unknown) {}
