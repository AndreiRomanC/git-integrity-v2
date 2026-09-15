(function attachLocalDiff(global) {
  const DEFAULT_MATRIX_LIMIT = 2_000_000;

  function splitLines(text) {
    return String(text ?? '').split('\n');
  }

  // Myers is substantially cheaper than the full LCS matrix when two large
  // source files are mostly alike (the normal folder-sync case). Work is
  // capped so two completely unrelated generated files cannot freeze the UI.
  function alignLinesMyers(leftLines, rightLines, workLimit = 500_000) {
    const n = leftLines.length;
    const m = rightLines.length;
    const maximum = n + m;
    let frontier = new Map([[1, 0]]);
    const trace = [];
    let work = 0;

    for (let distance = 0; distance <= maximum; distance++) {
      if (work + (distance * 2 + 1) > workLimit) return null;
      trace.push(new Map(frontier));
      for (let diagonal = -distance; diagonal <= distance; diagonal += 2) {
        const down = frontier.get(diagonal + 1);
        const right = frontier.get(diagonal - 1);
        let leftIndex;
        if (diagonal === -distance || (diagonal !== distance && (right ?? -1) < (down ?? -1))) {
          leftIndex = down ?? 0;
        } else {
          leftIndex = (right ?? 0) + 1;
        }
        let rightIndex = leftIndex - diagonal;
        while (leftIndex < n && rightIndex < m && leftLines[leftIndex] === rightLines[rightIndex]) {
          leftIndex++;
          rightIndex++;
          work++;
        }
        frontier.set(diagonal, leftIndex);
        work++;
        if (leftIndex >= n && rightIndex >= m) {
          const rows = [];
          let x = n;
          let y = m;
          for (let depth = distance; depth >= 0; depth--) {
            const previous = trace[depth];
            const currentDiagonal = x - y;
            const previousDiagonal = currentDiagonal === -depth || (currentDiagonal !== depth && (previous.get(currentDiagonal - 1) ?? -1) < (previous.get(currentDiagonal + 1) ?? -1))
              ? currentDiagonal + 1 : currentDiagonal - 1;
            const previousX = previous.get(previousDiagonal) ?? 0;
            const previousY = previousX - previousDiagonal;
            while (x > previousX && y > previousY) {
              rows.push({ left: leftLines[x - 1], right: rightLines[y - 1], same: true });
              x--;
              y--;
            }
            if (depth === 0) break;
            if (x === previousX) {
              rows.push({ left: null, right: rightLines[y - 1], same: false });
              y--;
            } else {
              rows.push({ left: leftLines[x - 1], right: null, same: false });
              x--;
            }
          }
          return rows.reverse();
        }
      }
    }
    return null;
  }

  function alignLines(leftLines, rightLines, matrixLimit = DEFAULT_MATRIX_LIMIT) {
    const n = leftLines.length;
    const m = rightLines.length;
    if (n * m > matrixLimit) {
      const exactRows = alignLinesMyers(leftLines, rightLines);
      if (exactRows) return { approximate: false, rows: exactRows };
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

  // The comparison view and the editor deliberately share the same aligned
  // row model. `null` is a visual alignment slot (it is not written to disk),
  // while an empty string is a real empty line. Keeping those two states
  // distinct is what lets a user insert a line on one side without shifting
  // the other file, then copy a source line into that exact slot.
  function createMergeRows(leftText, rightText, matrixLimit = DEFAULT_MATRIX_LIMIT) {
    return buildLineDiff(leftText, rightText, matrixLimit).rows.map((row, index) => ({
      id: index + 1,
      left: row.left,
      right: row.right,
    }));
  }

  function rowsToText(rows, side) {
    if (side !== 'left' && side !== 'right') throw new Error(`Unknown merge side: ${side}`);
    return rows
      .map(row => row[side])
      .filter(line => line !== null)
      .join('\n');
  }

  function cloneRows(rows) {
    return rows.map(row => ({ ...row }));
  }

  function normalizeRows(rows) {
    const normalized = rows.filter(row => row.left !== null || row.right !== null);
    return normalized.length ? normalized : [{ id: 1, left: '', right: '' }];
  }

  function nextRowId(rows) {
    return rows.reduce((maximum, row) => Math.max(maximum, Number(row.id) || 0), 0) + 1;
  }

  function setMergeLine(rows, rowIndex, side, value) {
    if (side !== 'left' && side !== 'right') throw new Error(`Unknown merge side: ${side}`);
    if (!rows[rowIndex]) return cloneRows(rows);
    const updated = cloneRows(rows);
    const lines = String(value ?? '').replace(/\r\n?/g, '\n').split('\n');
    updated[rowIndex][side] = lines.shift();
    let id = nextRowId(updated);
    for (const line of lines) {
      updated.splice(++rowIndex, 0, { id: id++, left: side === 'left' ? line : null, right: side === 'right' ? line : null });
    }
    return normalizeRows(updated);
  }

  function insertMergeLine(rows, rowIndex, side, column = 0) {
    if (side !== 'left' && side !== 'right') throw new Error(`Unknown merge side: ${side}`);
    if (!rows[rowIndex]) return { rows: cloneRows(rows), focusRow: rowIndex, focusColumn: 0 };
    const updated = cloneRows(rows);
    const current = updated[rowIndex][side] ?? '';
    const splitAt = Math.max(0, Math.min(Number(column) || 0, current.length));
    updated[rowIndex][side] = current.slice(0, splitAt);
    const inserted = { id: nextRowId(updated), left: null, right: null };
    inserted[side] = current.slice(splitAt);
    updated.splice(rowIndex + 1, 0, inserted);
    return { rows: updated, focusRow: rowIndex + 1, focusColumn: 0 };
  }

  function joinMergeLineBackward(rows, rowIndex, side) {
    if (side !== 'left' && side !== 'right') throw new Error(`Unknown merge side: ${side}`);
    if (!rows[rowIndex] || rows[rowIndex][side] === null) return { rows: cloneRows(rows), focusRow: rowIndex, focusColumn: 0 };
    let previous = rowIndex - 1;
    while (previous >= 0 && rows[previous][side] === null) previous--;
    if (previous < 0) return { rows: cloneRows(rows), focusRow: rowIndex, focusColumn: 0 };
    const updated = cloneRows(rows);
    const prefix = updated[previous][side] ?? '';
    updated[previous][side] = prefix + (updated[rowIndex][side] ?? '');
    updated[rowIndex][side] = null;
    const normalized = normalizeRows(updated);
    const focusRow = normalized.findIndex(row => row.id === updated[previous].id);
    return { rows: normalized, focusRow, focusColumn: prefix.length };
  }

  function copyMergeRow(rows, rowIndex, direction = 'left-to-right') {
    if (!rows[rowIndex]) return cloneRows(rows);
    const updated = cloneRows(rows);
    if (direction === 'left-to-right') updated[rowIndex].right = updated[rowIndex].left;
    else if (direction === 'right-to-left') updated[rowIndex].left = updated[rowIndex].right;
    else throw new Error(`Unknown merge direction: ${direction}`);
    return normalizeRows(updated);
  }

  function rowState(row) {
    if (row.left === row.right) return 'same';
    if (row.left === null) return 'right-only';
    if (row.right === null) return 'left-only';
    return 'changed';
  }

  const api = {
    buildLineDiff, mergeHunk, splitLines,
    createMergeRows, rowsToText, setMergeLine, insertMergeLine,
    joinMergeLineBackward, copyMergeRow, rowState,
  };
  global.LocalDriveDiff = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
