// Not a declared dependency and not an alias: resolved by a guess (Heuristic).
import { cfg } from 'config/app';
import { add } from '../packages/shared/src/math';
export const port = cfg.port + add(0, 0);
