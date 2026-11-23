import express from 'express';
import { lookupUser } from './tools';

const app = express();
const r = express.Router();
r.get('/health', (_req, res) => res.send('ok'));
r.get('/user/:id', async (req, res) => res.json(await lookupUser(Number(req.params.id))));
app.use('/v1', r);
app.listen(3000);
