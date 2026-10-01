// Generated artifact writer; no runtime methods are registered.
import fs from 'node:fs';
import { apiSchema } from './api-shapes.mjs';
const neutral=JSON.parse(fs.readFileSync(new URL('broker.schema.json',import.meta.url),'utf8'));
fs.writeFileSync(new URL('api.schema.json',import.meta.url),JSON.stringify(apiSchema(neutral.$defs),null,2)+'\n');
