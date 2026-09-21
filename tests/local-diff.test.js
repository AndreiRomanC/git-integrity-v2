const test = require('node:test');
const assert = require('node:assert/strict');
const {
  buildLineDiff, mergeHunk, mergeRow, createMergeRows, rowsToText, setMergeLine,
  insertMergeLine, joinMergeLineBackward, copyMergeRow, rowState,
} = require('../frontend/local-diff.js');

test('identical Local Drive files are reported as identical', () => {
  const result = buildLineDiff('one\ntwo\n', 'one\ntwo\n');
  assert.equal(result.identical, true);
  assert.equal(result.hunks.length, 0);
  assert.ok(result.rows.every(row => row.same));
});

test('insertions are aligned without marking the remaining file as changed', () => {
  const result = buildLineDiff('one\ntwo\nthree', 'one\ninserted\ntwo\nthree');
  assert.equal(result.hunks.length, 1);
  assert.deepEqual(result.rows.map(row => [row.left, row.right, row.same]), [
    ['one', 'one', true],
    [null, 'inserted', false],
    ['two', 'two', true],
    ['three', 'three', true],
  ]);
});

test('a changed block can be accepted in either direction without touching disk', () => {
  const left = 'header\nleft value\nfooter';
  const right = 'header\nright value\nfooter';
  assert.equal(mergeHunk(left, right, 0, 'left-to-right').rightText, left);
  assert.equal(mergeHunk(left, right, 0, 'right-to-left').leftText, right);
});

test('a line merge replaces only the aligned line and preserves the rest of the result', () => {
  const left = 'git-stress seed=';
  const right = 'git-stress seed=5eed5eedd15ca1fc revision=00000000\nsynthetic git';
  const diff = buildLineDiff(left, right);
  assert.deepEqual(diff.rows.map(row => [row.left, row.right]), [
    ['git-stress seed=', 'git-stress seed=5eed5eedd15ca1fc revision=00000000'],
    [null, 'synthetic git'],
  ]);
  assert.equal(mergeRow(left, right, 0, 'left-to-right').rightText, 'git-stress seed=\nsynthetic git');
  assert.equal(mergeHunk(left, right, 0, 'left-to-right').rightText, left);
});

test('line merge inserts or removes one aligned line without changing neighboring lines', () => {
  assert.equal(mergeRow('one\ninserted\ntwo', 'one\ntwo', 1, 'left-to-right').rightText, 'one\ninserted\ntwo');
  assert.equal(mergeRow('one\ntwo', 'one\nextra\ntwo', 1, 'left-to-right').rightText, 'one\ntwo');
});

test('comparison rules classify unimportant differences without changing original text', () => {
  const whitespace = buildLineDiff('  Alpha beta  ', 'Alpha   beta', undefined, 'ignore-whitespace');
  assert.equal(whitespace.equivalent, true);
  assert.equal(whitespace.identical, false);
  assert.equal(whitespace.ignoredDifferences, 1);
  assert.equal(whitespace.rows[0].ignored, true);
  assert.equal(rowsToText(whitespace.rows, 'left'), '  Alpha beta  ');
  assert.equal(rowsToText(whitespace.rows, 'right'), 'Alpha   beta');

  const letterCase = buildLineDiff('VALUE', 'value', undefined, 'ignore-case');
  assert.equal(letterCase.equivalent, true);
  assert.equal(letterCase.ignoredDifferences, 1);
});

test('default file alignment treats CRLF and LF as the same lines', () => {
  const result = buildLineDiff('{\r\n  "build": []\r\n}', '{\n  "build": []\n}');
  assert.equal(result.equivalent, true);
  assert.equal(result.hunks.length, 0);
  assert.equal(result.ignoredDifferences, 2);
  assert.deepEqual(result.rows.map(row => [row.leftNumber, row.rightNumber, row.same, row.ignored]), [
    [1, 1, true, true],
    [2, 2, true, true],
    [3, 3, true, false],
  ]);
});

test('large but similar comparisons keep exact alignment without the LCS matrix', () => {
  const result = buildLineDiff('a\nb\nc', 'a\nx\nc', 1);
  assert.equal(result.approximate, false);
  assert.equal(result.hunks.length, 1);
});

test('one insertion in a multi-thousand-line file does not mark the rest as changed', () => {
  const source = Array.from({ length: 3000 }, (_, index) => `line ${index}`);
  const result = [...source];
  result.splice(1500, 0, 'one inserted line');
  const diff = buildLineDiff(source.join('\n'), result.join('\n'));
  assert.equal(diff.approximate, false);
  assert.equal(diff.hunks.length, 1);
  assert.equal(diff.rows.filter(row => !row.same).length, 1);
  assert.deepEqual([diff.rows[1500].left, diff.rows[1500].right], [null, 'one inserted line']);
});

test('bounded Myers alignment always reconstructs both inputs across mixed edits', () => {
  let seed = 0x5eed1234;
  const random = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 0x100000000);
  for (let sample = 0; sample < 120; sample++) {
    const left = Array.from({ length: 80 }, (_, index) => `S${sample}-line-${index}`);
    const right = [...left];
    for (let edit = 0; edit < 8; edit++) {
      const index = Math.floor(random() * (right.length + 1));
      const action = Math.floor(random() * 3);
      if (action === 0) right.splice(index, 0, `insert-${sample}-${edit}`);
      else if (action === 1 && index < right.length) right.splice(index, 1);
      else if (index < right.length) right[index] = `replace-${sample}-${edit}`;
    }
    const result = buildLineDiff(left.join('\n'), right.join('\n'), 1);
    assert.equal(rowsToText(result.rows, 'left'), left.join('\n'));
    assert.equal(rowsToText(result.rows, 'right'), right.join('\n'));
    assert.ok(result.rows.every(row => !row.same || row.left === row.right));
  }
});

test('aligned editing preserves placeholders separately from real blank lines', () => {
  const rows = createMergeRows('one\ntwo\nthree', 'one\ninserted\ntwo\nthree');
  assert.deepEqual(rows.map(row => [row.left, row.right]), [
    ['one', 'one'], [null, 'inserted'], ['two', 'two'], ['three', 'three'],
  ]);
  assert.equal(rowsToText(rows, 'left'), 'one\ntwo\nthree');
  assert.equal(rowsToText(rows, 'right'), 'one\ninserted\ntwo\nthree');

  const withRealBlank = setMergeLine(rows, 1, 'left', '');
  assert.equal(rowsToText(withRealBlank, 'left'), 'one\n\ntwo\nthree');
  assert.equal(rowState(withRealBlank[1]), 'changed');
});

test('Enter inserts a line only on the edited side and keeps vertical alignment', () => {
  const rows = createMergeRows('alpha beta\nomega', 'alpha beta\nomega');
  const inserted = insertMergeLine(rows, 0, 'right', 5);
  assert.deepEqual(inserted.rows.map(row => [row.left, row.right]), [
    ['alpha beta', 'alpha'], [null, ' beta'], ['omega', 'omega'],
  ]);
  assert.equal(rowsToText(inserted.rows, 'left'), 'alpha beta\nomega');
  assert.equal(rowsToText(inserted.rows, 'right'), 'alpha\n beta\nomega');
});

test('a source row can be copied into an aligned destination slot', () => {
  const rows = createMergeRows('header\nsource line\nfooter', 'header\nfooter');
  const sourceIndex = rows.findIndex(row => row.left === 'source line');
  assert.equal(rows[sourceIndex].right, null);
  const copied = copyMergeRow(rows, sourceIndex, 'left-to-right');
  assert.equal(rowsToText(copied, 'right'), 'header\nsource line\nfooter');
  assert.equal(rowState(copied[sourceIndex]), 'same');
});

test('Backspace at the beginning joins only the active side', () => {
  const rows = createMergeRows('first\nsecond', 'first\nsecond');
  const joined = joinMergeLineBackward(rows, 1, 'right');
  assert.equal(rowsToText(joined.rows, 'right'), 'firstsecond');
  assert.equal(rowsToText(joined.rows, 'left'), 'first\nsecond');
  assert.equal(joined.focusColumn, 5);
});
