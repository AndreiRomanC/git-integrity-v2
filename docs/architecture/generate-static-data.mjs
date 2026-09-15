// Generates the file:// fallback from the canonical architecture.json model.
// Run after changing the JSON: node generate-static-data.mjs
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const directory = dirname(fileURLToPath(import.meta.url));
const json = await readFile(join(directory, 'architecture.json'), 'utf8');
const model = JSON.parse(json);
const output = `/* GENERATED from architecture.json — do not edit by hand. */\nwindow.__ARCHITECTURE_MODEL__ = ${JSON.stringify(model)};\n`;
await writeFile(join(directory, 'architecture-data.js'), output, 'utf8');
console.log('Generated architecture-data.js from architecture.json');
