import express from 'express';
import { jobs } from './jobs';

const app = express();
// One router mounted twice: its routes under both prefixes.
app.use('/v1', jobs);
app.use('/v2', jobs);
app.get('/health', (_req, res) => res.send('ok'));
app.listen(3000);
