const test = require('node:test');
const assert = require('node:assert/strict');
const { buildLineDiff, mergeHunk } = require('../frontend/local-diff.js');

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

test('large comparisons use the bounded fallback', () => {
  const result = buildLineDiff('a\nb\nc', 'a\nx\nc', 1);
  assert.equal(result.approximate, true);
  assert.equal(result.hunks.length, 1);
});
