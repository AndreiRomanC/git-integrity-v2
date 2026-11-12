const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const read = file => fs.readFileSync(path.join(__dirname, '..', file), 'utf8');

test('Windows builds keep the default console for the app and Git commands', () => {
  assert.doesNotMatch(read('src-tauri/src/main.rs'), /windows_subsystem\s*=\s*"windows"/);
  const repository = read('src-tauri/src/repository.rs').split('\nmod tests {')[0];
  const terminal = read('src-tauri/src/repository/command_console.rs').split('\nmod tests {')[0];
  for (const source of [repository, terminal]) {
    assert.match(source, /Command::new\("git"\)/);
    assert.doesNotMatch(source, /CREATE_NO_WINDOW|0x08000000/i);
  }
  assert.match(terminal, /Command::new\(shell\)/);
});

test('intentional external tool consoles are not suppressed', () => {
  const repository = read('src-tauri/src/repository.rs');
  const utrud = repository.match(/fn run_utrud\([^]*?\n}/)?.[0];
  assert.ok(utrud, 'UTRUD implementation exists');
  assert.match(utrud, /Command::new\("cmd"\)/);
  const merge = read('src-tauri/src/repository/branches/merge.rs');
  assert.match(merge, /Command::new\("git"\)\s*\.args\(\["mergetool"/);
});
