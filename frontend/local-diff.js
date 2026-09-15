(function attachLocalDiff(global) {
  const DEFAULT_MATRIX_LIMIT = 2_000_000;

  function splitLines(text) {
    return String(text ?? '').split('\n');
  }

  function alignLines(leftLines, rightLines, matrixLimit = DEFAULT_MATRIX_LIMIT) {
    const n = leftLines.length;
    const m = rightLines.length;
    if (n * m > matrixLimit) {
      const count = Math.max(n, m);
      return {
        approximate: true,
        rows: Array.from({ length: count }, (_, index) => ({
          left: index < n ? leftLines[index] : null,
          right: index < m ? rightLines[index] : null,
          same: index < n && index < m && leftLines[index] === rightLines[index],
        })),
      };
    }

    const table = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
    for (let left = n - 1; left >= 0; left--) {
      for (let right = m - 1; right >= 0; right--) {
        table[left][right] = leftLines[left] === rightLines[right]
          ? table[left + 1][right + 1] + 1
          : Math.max(table[left + 1][right], table[left][right + 1]);
      }
    }

    const rows = [];
    let left = 0;
    let right = 0;
    while (left < n && right < m) {
      if (leftLines[left] === rightLines[right]) {
        rows.push({ left: leftLines[left++], right: rightLines[right++], same: true });
      } else if (table[left + 1][right] >= table[left][right + 1]) {
        rows.push({ left: leftLines[left++], right: null, same: false });
      } else {
        rows.push({ left: null, right: rightLines[right++], same: false });
      }
    }
    while (left < n) rows.push({ left: leftLines[left++], right: null, same: false });
    while (right < m) rows.push({ left: null, right: rightLines[right++], same: false });
    return { approximate: false, rows };
  }

  function buildLineDiff(leftText, rightText, matrixLimit = DEFAULT_MATRIX_LIMIT) {
    const leftLines = splitLines(leftText);
    const rightLines = splitLines(rightText);
    const aligned = alignLines(leftLines, rightLines, matrixLimit);
    const hunks = [];
    let currentHunk = null;
    let leftCursor = 0;
    let rightCursor = 0;
    let leftNumber = 0;
    let rightNumber = 0;

    const rows = aligned.rows.map(row => {
      if (row.same) {
        currentHunk = null;
      } else if (!currentHunk) {
        currentHunk = {
          leftStart: leftCursor,
          rightStart: rightCursor,
          leftLines: [],
          rightLines: [],
        };
        hunks.push(currentHunk);
      }

      const hunkIndex = row.same ? -1 : hunks.length - 1;
      const hunkFirst = !row.same && currentHunk.leftLines.length === 0 && currentHunk.rightLines.length === 0;
      if (row.left !== null) {
        leftCursor++;
        leftNumber++;
        if (currentHunk) currentHunk.leftLines.push(row.left);
      }
      if (row.right !== null) {
        rightCursor++;
        rightNumber++;
        if (currentHunk) currentHunk.rightLines.push(row.right);
      }
      return { ...row, leftNumber: row.left === null ? null : leftNumber, rightNumber: row.right === null ? null : rightNumber, hunkIndex, hunkFirst };
    });

    return {
      rows,
      hunks,
      identical: String(leftText ?? '') === String(rightText ?? ''),
      approximate: aligned.approximate,
    };
  }

  function mergeHunk(leftText, rightText, hunkIndex, direction, matrixLimit = DEFAULT_MATRIX_LIMIT) {
    const diff = buildLineDiff(leftText, rightText, matrixLimit);
    const hunk = diff.hunks[hunkIndex];
    if (!hunk) return { leftText, rightText };
    const leftLines = splitLines(leftText);
    const rightLines = splitLines(rightText);
    if (direction === 'left-to-right') {
      rightLines.splice(hunk.rightStart, hunk.rightLines.length, ...hunk.leftLines);
    } else if (direction === 'right-to-left') {
      leftLines.splice(hunk.leftStart, hunk.leftLines.length, ...hunk.rightLines);
    } else {
      throw new Error(`Unknown merge direction: ${direction}`);
    }
    return { leftText: leftLines.join('\n'), rightText: rightLines.join('\n') };
  }

  const api = { buildLineDiff, mergeHunk, splitLines };
  global.LocalDriveDiff = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
