const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const read = file => fs.readFileSync(path.join(__dirname, '..', file), 'utf8');

test('Windows release executable is graphical while development builds retain their console', () => {
  assert.match(read('src-tauri/src/main.rs'), /#!\[cfg_attr\(all\(windows, not\(debug_assertions\)\), windows_subsystem = "windows"\)\]/);
});

test('background Git, gh, credential and embedded terminal commands opt out of Windows consoles', () => {
  const repository = read('src-tauri/src/repository.rs').split('\nmod tests {')[0];
  assert.doesNotMatch(repository, /Command::new\("(?:git|gh|taskkill)"\)/);
  assert.ok(repository.includes('background_command("git")'));
  assert.ok(repository.includes('background_command("gh")'));
  const terminal = read('src-tauri/src/repository/command_console.rs').split('\nmod tests {')[0];
  assert.doesNotMatch(terminal, /Command::new\(/);
  assert.match(terminal, /background_command\(shell\)/);
  assert.match(read('src-tauri/src/repository/background_process.rs'), /creation_flags\(0x08000000\)/);
});

test('intentional external tool consoles are not suppressed', () => {
  const repository = read('src-tauri/src/repository.rs');
  const utrud = repository.match(/fn run_utrud\([^]*?\n}/)?.[0];
  assert.ok(utrud, 'UTRUD implementation exists');
  assert.match(utrud, /Command::new\("cmd"\)/);
  assert.doesNotMatch(utrud, /background_command/);
  const merge = read('src-tauri/src/repository/branches/merge.rs');
  assert.match(merge, /Command::new\("git"\)\s*\.args\(\["mergetool"/);
});
