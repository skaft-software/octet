// Fails for a reason unrelated to Pi exports: never fallback-eligible.
import { VERSION } from '@earendil-works/pi-coding-agent';
throw new Error(`factory module failed on purpose ${VERSION.length}`);
export default function () {}
