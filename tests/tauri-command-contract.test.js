const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');

function registeredCommands() {
  const source = fs.readFileSync(path.join(root, 'src-tauri/src/lib.rs'), 'utf8');
  const body = source.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1];
  assert.ok(body, 'the Tauri command registry must remain discoverable');
  return body
    .split(',')
    .map(entry => entry.trim())
    .filter(Boolean)
    .map(entry => entry.split('::').at(-1));
}

function frontendInvocations() {
  const frontend = path.join(root, 'frontend');
  const commands = new Set();
  for (const name of fs.readdirSync(frontend)) {
    if (!name.endsWith('.js')) continue;
    const source = fs.readFileSync(path.join(frontend, name), 'utf8');
    for (const match of source.matchAll(/\binvoke\(\s*['"]([A-Za-z0-9_]+)['"]/g)) {
      commands.add(match[1]);
    }
  }
  return [...commands].sort();
}

test('modularization preserves the complete Tauri command surface', () => {
  const commands = registeredCommands();
  assert.equal(new Set(commands).size, commands.length, 'Tauri command names must be unique even when handlers live in different modules');
  const expected = fs.readFileSync(path.join(root, 'tests/fixtures/tauri-command-surface.txt'), 'utf8')
    .trim().split(/\r?\n/).sort();
  assert.deepEqual([...commands].sort(), expected,
    'the public IPC command surface changed; update the snapshot only for an intentional API change');
});

test('every statically named frontend invoke still has a registered backend command', () => {
  const registered = new Set(registeredCommands());
  const missing = frontendInvocations().filter(command => !registered.has(command));
  assert.deepEqual(missing, []);
});
