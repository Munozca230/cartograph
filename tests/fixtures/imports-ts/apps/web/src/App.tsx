import { util } from './util';
import { helper } from './helper.js';
import type { Props } from './types';
export * from './reexport';
export { missing } from './missing';
export * as shapes from '@shared/fmt';

export function App(_props: Props) {
  return <div>{util()}{helper()}</div>;
}
