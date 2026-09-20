(function attachLocalDrive(global) {
  function escapeHtml(value = '') {
    return String(value).replace(/[&<>'"]/g, character => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' }[character]));
  }

  function formatBytes(bytes) {
    const value = Number(bytes || 0);
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
    if (value < 1024 * 1024 * 1024) return `${(value / (1024 * 1024)).toFixed(1)} MB`;
    return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function nativeParentPath(path) {
    const value = String(path || '').replace(/[\\/]+$/, '');
    if (!value) return '';
    const separator = value.includes('\\') ? '\\' : '/';
    const index = Math.max(value.lastIndexOf('/'), value.lastIndexOf('\\'));
    if (index < 0) return '';
    if (index === 0) return '/';
    if (/^[A-Za-z]:$/.test(value.slice(0, index))) return `${value.slice(0, index)}\\`;
    return value.slice(0, index).replace(/[\\/]+$/, '') || separator;
  }

  function nativeChildPath(root, relativePath) {
    const value = String(root || '').replace(/[\\/]+$/, '');
    const separator = value.includes('\\') ? '\\' : '/';
    const child = String(relativePath || '').replace(/[\\/]+/g, separator).replace(new RegExp(`^\\${separator}+`), '');
    if (!child) return value;
    return value ? `${value}${separator}${child}` : child;
  }

  function displayDate(seconds) {
    if (!seconds) return '';
    try { return new Date(seconds * 1000).toLocaleDateString(); }
    catch { return ''; }
  }

  function displayEncoding(encoding) {
    const labels = {
      'utf-8': 'UTF-8', 'utf-8-bom': 'UTF-8 with BOM',
      'utf-16le': 'UTF-16 LE', 'utf-16be': 'UTF-16 BE',
      'utf-16le-no-bom': 'UTF-16 LE without BOM', 'utf-16be-no-bom': 'UTF-16 BE without BOM',
      'windows-1252': 'Windows-1252 / ANSI',
    };
    return labels[encoding] || encoding || 'UTF-8';
  }

  function create(options) {
    const root = options.root;
    const invoke = options.invoke;
    const notify = options.notify || (() => {});
    const ask = options.confirm || (async message => global.confirm(message));
    const promptForName = options.prompt || (async message => global.prompt(message));
    const documentRef = options.document || global.document;
    const diffEngine = options.diff || global.LocalDriveDiff;
    const maxAlignedEditorRows = 12_000;
    const maxDiffPreviewRows = 5_000;
    const folderRowPageSize = 1_000;
    const sides = {
      left: { path: '', parent: null, entries: [], selectedPath: '', loading: false, generation: 0 },
      right: { path: '', parent: null, entries: [], selectedPath: '', loading: false, generation: 0 },
    };
    const editor = {
      dialog: documentRef?.querySelector('#localDriveEditorDialog'),
      title: documentRef?.querySelector('#localDriveEditorTitle'),
      path: documentRef?.querySelector('#localDriveEditorPath'),
      hint: documentRef?.querySelector('#localDriveEditorHint'),
      content: documentRef?.querySelector('#localDriveEditorContent'),
      state: documentRef?.querySelector('#localDriveEditorState'),
      save: documentRef?.querySelector('#saveLocalDriveEditor'),
      close: documentRef?.querySelector('#closeLocalDriveEditor'),
      cancel: documentRef?.querySelector('#cancelLocalDriveEditor'),
      filePath: '', original: '', encoding: 'utf-8', fingerprint: '', side: 'right', editing: false,
    };
    const comparison = {
      dialog: documentRef?.querySelector('#localDriveCompareDialog'),
      summary: documentRef?.querySelector('#localDriveCompareSummary'),
      leftPathNode: documentRef?.querySelector('#localDriveCompareLeftPath'),
      rightPathNode: documentRef?.querySelector('#localDriveCompareRightPath'),
      diffView: documentRef?.querySelector('#localDriveDiffView'),
      diffRows: documentRef?.querySelector('#localDriveDiffRows'),
      editors: documentRef?.querySelector('#localDriveMergeEditors'),
      alignedEditors: documentRef?.querySelector('#localDriveAlignedEditors'),
      resultPreview: documentRef?.querySelector('#localDriveResultPreview'),
      resultStats: documentRef?.querySelector('#localDriveResultStats'),
      showDiff: documentRef?.querySelector('#showLocalDriveDiff'),
      showEdit: documentRef?.querySelector('#editLocalDriveDiff'),
      previousDifference: documentRef?.querySelector('#previousLocalDriveDifference'),
      nextDifference: documentRef?.querySelector('#nextLocalDriveDifference'),
      rules: documentRef?.querySelector('#localDriveComparisonRules'),
      copyAllRight: documentRef?.querySelector('#copyAllLocalDriveRight'),
      undoResult: documentRef?.querySelector('#undoLocalDriveResult'),
      resetResult: documentRef?.querySelector('#resetLocalDriveResult'),
      saveLeft: documentRef?.querySelector('#saveLocalDriveCompareLeft'),
      saveRight: documentRef?.querySelector('#saveLocalDriveCompareRight'),
      stateNode: documentRef?.querySelector('#localDriveCompareState'),
      close: documentRef?.querySelector('#closeLocalDriveCompare'),
      cancel: documentRef?.querySelector('#cancelLocalDriveCompare'),
      leftPath: '', rightPath: '', leftOriginal: '', rightOriginal: '',
      leftText: '', rightText: '', leftEncoding: 'utf-8', rightEncoding: 'utf-8',
      leftFingerprint: '', rightFingerprint: '', mergeRows: [], resultHistory: [],
      mode: 'diff', generation: 0, loading: false, returnToFolderMerge: false,
      activeDifferenceRow: -1, readOnly: false,
    };
    const folderMerge = {
      dialog: documentRef?.querySelector('#localFolderMergeDialog'),
      title: documentRef?.querySelector('#localFolderMergeTitle'),
      summary: documentRef?.querySelector('#localFolderMergeSummary'),
      compareMode: documentRef?.querySelector('#localFolderCompareMode'),
      guidedMode: documentRef?.querySelector('#localFolderGuidedMode'),
      directionWrap: documentRef?.querySelector('#localFolderDirectionWrap'),
      direction: documentRef?.querySelector('#localFolderMergeDirection'),
      ignoreSubmodules: documentRef?.querySelector('#ignoreLocalFolderSubmodules'),
      leftPathNode: documentRef?.querySelector('#localFolderMergeLeftPath'),
      rightPathNode: documentRef?.querySelector('#localFolderMergeRightPath'),
      counts: documentRef?.querySelector('#localFolderMergeCounts'),
      rows: documentRef?.querySelector('#localFolderMergeRows'),
      queueHint: documentRef?.querySelector('#localFolderQueueHint'),
      previewTitle: documentRef?.querySelector('#localFolderPreviewTitle'),
      previewStatus: documentRef?.querySelector('#localFolderPreviewStatus'),
      previewContent: documentRef?.querySelector('#localFolderPreviewContent'),
      stateNode: documentRef?.querySelector('#localFolderMergeState'),
      rescan: documentRef?.querySelector('#rescanLocalFolderMerge'),
      skip: documentRef?.querySelector('#skipLocalFolderMergeFile'),
      review: documentRef?.querySelector('#reviewLocalFolderMergeFile'),
      apply: documentRef?.querySelector('#applyLocalFolderMergeFile'),
      close: documentRef?.querySelector('#closeLocalFolderMerge'),
      cancel: documentRef?.querySelector('#cancelLocalFolderMerge'),
      leftRoot: '', rightRoot: '', entries: [], selectedPath: '', loading: false, mode: 'compare', visibleLimit: folderRowPageSize,
      generation: 0, previewGeneration: 0, resolved: new Set(), skipped: new Set(),
    };
    let active = false;
    let initializedRepository = '';
    let activeSide = 'right';
    let filter = '';
    let operationBusy = false;
    let lastFolderMergeClick = { path: '', time: 0 };

    function paneFor(side) { return root?.querySelector(`[data-drive-side="${side}"]`); }
    function selectedEntry(side = activeSide) { return sides[side].entries.find(entry => entry.path === sides[side].selectedPath) || null; }
    function otherSide(side = activeSide) { return side === 'left' ? 'right' : 'left'; }
    function icon(entry) { return entry.kind === 'folder' ? '▰' : entry.kind === 'symlink' ? '↗' : '▤'; }

    function comparisonPair() {
      const left = selectedEntry('left');
      const right = selectedEntry('right');
      if (left?.kind === 'file' && right?.kind === 'file') return { left, right };
      const source = selectedEntry(activeSide);
      if (source?.kind !== 'file') return null;
      const targetSide = otherSide(activeSide);
      const match = sides[targetSide].entries.find(entry => entry.kind === 'file' && entry.name === source.name);
      if (!match) return null;
      return activeSide === 'left' ? { left: source, right: match } : { left: match, right: source };
    }

    function renderCommandState() {
      const selected = selectedEntry();
      const destination = sides[otherSide()];
      const setAvailability = (action, unavailable) => {
        const button = root?.querySelector(`[data-drive-shortcut="${action}"]`);
        // Total Commander keeps its function-key bar visually fixed while a
        // pane navigates. aria-disabled communicates availability without
        // causing the whole bottom bar to flash between opaque/faded states;
        // every action still repeats the same guard before touching disk.
        if (button) button.setAttribute('aria-disabled', String(operationBusy || unavailable));
      };
      setAvailability('compare', !comparisonPair() || sides.left.loading || sides.right.loading || !diffEngine);
      setAvailability('view', selected?.kind !== 'file');
      setAvailability('edit', selected?.kind !== 'file');
      setAvailability('copy', !selected || selected.kind === 'symlink' || !destination.path || sides.left.loading || sides.right.loading);
      setAvailability('move', !selected || !destination.path || sides.left.loading || sides.right.loading);
      setAvailability('mkdir', !sides[activeSide].path || sides[activeSide].loading);
      setAvailability('trash', !selected || sides[activeSide].loading);
      setAvailability('folder-compare', !sides.left.path || !sides.right.path || sides.left.loading || sides.right.loading);
      setAvailability('folder-merge', !sides.left.path || !sides.right.path || sides.left.loading || sides.right.loading);
    }

    function renderActivePane() {
      for (const side of ['left', 'right']) paneFor(side)?.classList.toggle('active', side === activeSide);
      renderCommandState();
    }

    function renderSelection(side) {
      const pane = paneFor(side);
      pane?.querySelectorAll('[data-drive-entry]').forEach(button => {
        const entry = sides[side].entries[Number(button.dataset.driveEntry)];
        button.classList.toggle('selected', entry?.path === sides[side].selectedPath);
      });
      renderCommandState();
    }

    function renderSide(side) {
      const model = sides[side];
      const pane = paneFor(side);
      if (!pane) return;
      const pathNode = pane.querySelector('[data-drive-path]');
      pathNode.textContent = model.path || 'No folder selected';
      pathNode.title = model.path || 'No folder selected';
      pane.querySelector('[data-drive-action="refresh"]').disabled = model.loading || !model.path;
      const list = pane.querySelector('[data-drive-list]');
      if (model.loading) {
        list.innerHTML = '<div class="local-drive-empty"><i class="spinner"></i> Loading folder…</div>';
        return;
      }
      if (!model.path) {
        list.innerHTML = '<div class="local-drive-empty">Choose a folder.</div>';
        return;
      }
      const query = filter.trim().toLowerCase();
      const entries = model.entries.filter(entry => !query || entry.name.toLowerCase().includes(query));
      const parent = model.parent ? '<button type="button" class="local-drive-entry parent-entry" data-drive-up="1" title="Parent folder"><span class="entry-icon">↰</span><span class="local-drive-entry-copy"><strong>..</strong></span><span class="local-drive-entry-kind">parent</span><span></span></button>' : '';
      const rows = entries.map(entry => {
        const index = model.entries.indexOf(entry);
        const detail = entry.kind === 'file' ? formatBytes(entry.size) : entry.kind;
        return `<button type="button" class="local-drive-entry ${entry.path === model.selectedPath ? 'selected' : ''}" data-drive-entry="${index}" title="${escapeHtml(entry.path)}"><span class="entry-icon ${escapeHtml(entry.kind)}">${icon(entry)}</span><span class="local-drive-entry-copy"><strong>${escapeHtml(entry.name)}</strong></span><span class="local-drive-entry-kind">${escapeHtml(detail)}</span><span class="local-drive-entry-time">${escapeHtml(displayDate(entry.modified))}</span></button>`;
      }).join('');
      list.innerHTML = parent + rows || '<div class="local-drive-empty">This folder is empty.</div>';
    }

    function render() {
      if (!active) return;
      renderSide('left');
      renderSide('right');
      renderActivePane();
    }

    async function load(side, path, { preserveRows = false } = {}) {
      if (!invoke || !path) return null;
      const model = sides[side];
      const generation = ++model.generation;
      model.loading = true;
      if (!preserveRows) {
        model.selectedPath = '';
        renderSide(side);
      }
      renderCommandState();
      try {
        const result = await invoke('list_local_directory', { path });
        if (generation !== model.generation) return null;
        model.path = result.path;
        model.parent = result.parent || null;
        model.entries = result.entries || [];
        return result;
      } catch (error) {
        if (generation === model.generation) notify(String(error), 'error');
        return null;
      } finally {
        if (generation === model.generation) {
          model.loading = false;
          renderSide(side);
          renderActivePane();
        }
      }
    }

    async function choose(side) {
      activeSide = side;
      renderActivePane();
      if (!invoke) return notify('Local Drive is available in the desktop application.', 'error');
      try {
        const path = await invoke('choose_folder');
        if (path) await load(side, path);
      } catch (error) { notify(String(error), 'error'); }
    }

    async function openRepositoryLocation(repositoryPath, relativePath = '') {
      if (!repositoryPath || !invoke) return;
      active = true;
      initializedRepository = repositoryPath;
      activeSide = 'right';
      const targetPath = nativeChildPath(repositoryPath, relativePath);
      const hadLeftPane = Boolean(sides.left.path);
      const right = sides.right.path === targetPath ? { path: sides.right.path, parent: sides.right.parent } : await load('right', targetPath);
      if (!hadLeftPane) {
        const leftStart = right?.parent || nativeParentPath(targetPath);
        if (leftStart) await load('left', leftStart);
      }
      render();
    }

    async function createFolder() {
      const model = sides[activeSide];
      if (!model.path || model.loading || operationBusy) return;
      operationBusy = true; renderCommandState();
      const name = await promptForName(`New folder inside:\n${model.path}`, '', { title: 'New folder', okLabel: 'Create' });
      if (name === null) { operationBusy = false; renderCommandState(); return; }
      try {
        await invoke('create_local_directory', { parentPath: model.path, name });
        notify(`Created folder “${String(name).trim()}”`);
        await load(activeSide, model.path, { preserveRows: true });
        options.onMutation?.([model.path]);
      } catch (error) { notify(String(error), 'error'); }
      finally { operationBusy = false; renderCommandState(); }
    }

    async function copySelected() {
      const sourceSide = activeSide;
      const destinationSide = otherSide(sourceSide);
      const source = selectedEntry(sourceSide);
      const destination = sides[destinationSide];
      if (!source || source.kind === 'symlink' || !destination.path || operationBusy || sides.left.loading || sides.right.loading) return;
      operationBusy = true; renderCommandState();
      if (!await ask(`From: ${source.path}\n\nTo: ${destination.path}\n\nExisting items are never overwritten.`, { title: 'Copy to the other pane', okLabel: 'Copy' })) { operationBusy = false; renderCommandState(); return; }
      try {
        const result = await invoke('copy_local_item', { sourcePath: source.path, destinationDirectory: destination.path });
        notify(`Copied ${result.files} file${result.files === 1 ? '' : 's'} (${formatBytes(result.bytes)})`);
        await load(destinationSide, destination.path, { preserveRows: true });
        options.onMutation?.([result.destination]);
      } catch (error) {
        await Promise.all([
          load(sourceSide, sides[sourceSide].path, { preserveRows: true }),
          load(destinationSide, destination.path, { preserveRows: true }),
        ]);
        notify(String(error), 'error');
      }
      finally { operationBusy = false; renderCommandState(); }
    }

    async function moveSelected() {
      const sourceSide = activeSide;
      const destinationSide = otherSide(sourceSide);
      const source = selectedEntry(sourceSide);
      const destination = sides[destinationSide];
      if (!source || !destination.path || operationBusy || sides.left.loading || sides.right.loading) return;
      operationBusy = true; renderCommandState();
      if (!await ask(`From: ${source.path}\n\nTo: ${destination.path}\n\nExisting items are never overwritten.`, { title: 'Move to the other pane', okLabel: 'Move' })) { operationBusy = false; renderCommandState(); return; }
      try {
        const result = await invoke('move_local_item', { sourcePath: source.path, destinationDirectory: destination.path });
        notify(`Moved “${source.name}” to ${destination.path}`);
        await Promise.all([
          load(sourceSide, sides[sourceSide].path, { preserveRows: true }),
          load(destinationSide, destination.path, { preserveRows: true }),
        ]);
        options.onMutation?.([source.path, result.destination]);
      } catch (error) {
        await Promise.all([
          load(sourceSide, sides[sourceSide].path, { preserveRows: true }),
          load(destinationSide, destination.path, { preserveRows: true }),
        ]);
        notify(String(error), 'error');
      }
      finally { operationBusy = false; renderCommandState(); }
    }

    async function moveSelectedToTrash() {
      const model = sides[activeSide];
      const entry = selectedEntry();
      if (!entry || model.loading || operationBusy) return;
      operationBusy = true; renderCommandState();
      if (!await ask(`${entry.path}\n\nThe item will be moved to Trash/Recycle Bin. This does not run git rm or create a commit.`, { title: 'Delete local item', danger: true, okLabel: 'Move to Trash' })) { operationBusy = false; renderCommandState(); return; }
      try {
        await invoke('trash_local_item', { path: entry.path });
        notify(`Moved “${entry.name}” to Trash/Recycle Bin`);
        const changedPath = entry.path;
        await load(activeSide, model.path, { preserveRows: true });
        options.onMutation?.([changedPath]);
      } catch (error) { notify(String(error), 'error'); }
      finally { operationBusy = false; renderCommandState(); }
    }

    async function openTextFile(editing) {
      const entry = selectedEntry();
      if (!entry || entry.kind !== 'file' || !editor.dialog || operationBusy) return;
      editor.filePath = entry.path;
      editor.side = activeSide;
      editor.editing = editing;
      editor.title.textContent = editing ? `Edit ${entry.name}` : `View ${entry.name}`;
      editor.path.textContent = entry.path;
      editor.hint.textContent = editing ? 'Local Drive text editor · UTF-8 · maximum 2 MB' : 'Read-only Local Drive viewer · UTF-8 · maximum 2 MB';
      editor.content.value = 'Loading…';
      editor.content.readOnly = true;
      editor.save.hidden = !editing;
      editor.save.disabled = true;
      editor.state.textContent = 'Loading…';
      editor.dialog.showModal();
      try {
        const file = await invoke('read_local_text_file', { path: entry.path });
        if (editor.filePath !== entry.path) return;
        editor.original = file.content;
        editor.encoding = file.encoding || 'utf-8';
        editor.fingerprint = file.fingerprint || '';
        editor.content.value = file.content;
        editor.content.readOnly = !editing;
        editor.save.disabled = true;
        editor.hint.textContent = `${editing ? 'Local Drive text editor' : 'Read-only Local Drive viewer'} · ${displayEncoding(editor.encoding)} · maximum 2 MB`;
        editor.state.textContent = editing ? `Saved locally · ${displayEncoding(editor.encoding)} · no changes` : `${formatBytes(file.bytes)} · ${displayEncoding(editor.encoding)} · read only`;
        editor.content.focus();
      } catch (error) {
        editor.dialog.close();
        notify(String(error), 'error');
      }
    }

    async function saveTextFile() {
      if (!editor.editing || !editor.filePath || editor.content.value === editor.original || operationBusy) return;
      const path = editor.filePath;
      operationBusy = true;
      editor.save.disabled = true;
      editor.state.textContent = 'Saving…';
      try {
        const result = await invoke('write_local_text_file', {
          path, content: editor.content.value, encoding: editor.encoding,
          expectedFingerprint: editor.fingerprint || null,
        });
        editor.original = editor.content.value;
        editor.fingerprint = result?.fingerprint || editor.fingerprint;
        editor.state.textContent = 'Saved locally';
        notify(`Saved ${path}`);
        await load(editor.side, sides[editor.side].path, { preserveRows: true });
        options.onMutation?.([path]);
      } catch (error) {
        editor.state.textContent = 'Save failed';
        notify(String(error), 'error');
      } finally {
        operationBusy = false;
        editor.save.disabled = editor.content.value === editor.original;
        renderCommandState();
      }
    }

    async function closeTextEditor() {
      if (editor.editing && editor.content.value !== editor.original && !await ask('Discard the unsaved edits in this file?', { title: 'Unsaved edits', danger: true, okLabel: 'Discard' })) return;
      editor.dialog.close();
    }

    function comparisonDirty(side) {
      return comparison[`${side}Text`] !== comparison[`${side}Original`];
    }

    function syncComparisonEditors() {
      if (comparison.mode !== 'edit' || !comparison.mergeRows.length) return;
      comparison.leftText = diffEngine.rowsToText(comparison.mergeRows, 'left');
      comparison.rightText = diffEngine.rowsToText(comparison.mergeRows, 'right');
    }

    function rebuildMergeRows() {
      comparison.mergeRows = diffEngine.createMergeRows(comparison.leftText, comparison.rightText);
    }

    function rememberRightResult() {
      syncComparisonEditors();
      if (comparison.resultHistory.at(-1) === comparison.rightText) return;
      comparison.resultHistory.push(comparison.rightText);
      if (comparison.resultHistory.length > 50) comparison.resultHistory.shift();
    }

    function renderResultPreview() {
      syncComparisonEditors();
      if (comparison.resultPreview) comparison.resultPreview.textContent = comparison.rightText;
      if (comparison.resultStats) {
        const lines = String(comparison.rightText).split('\n').length;
        comparison.resultStats.textContent = `${lines.toLocaleString()} line${lines === 1 ? '' : 's'} · ${comparisonDirty('right') ? 'prepared, not saved' : 'matches saved file'}`;
      }
    }

    function mergeLineNumber(rows, rowIndex, side) {
      let number = 0;
      for (let index = 0; index <= rowIndex; index++) if (rows[index][side] !== null) number++;
      return rows[rowIndex][side] === null ? '' : number;
    }

    function renderMergeCell(row, rowIndex, side) {
      const value = row[side];
      const state = diffEngine.rowState(row);
      const number = mergeLineNumber(comparison.mergeRows, rowIndex, side);
      const sideState = state === 'same' ? 'same' : state === 'changed' ? 'changed' : state;
      if (value === null) {
        return `<div class="local-drive-edit-cell ${side} ${sideState}" role="gridcell"><span class="local-drive-edit-number"></span><button type="button" class="local-drive-line-placeholder" data-local-line-create="${side}" data-row="${rowIndex}" title="Create a real empty line in this alignment slot">+ create line here</button></div>`;
      }
      return `<label class="local-drive-edit-cell ${side} ${sideState}" role="gridcell"><span class="local-drive-edit-number">${number}</span><textarea rows="1" class="local-drive-line-input" data-local-line-side="${side}" data-row="${rowIndex}" spellcheck="false" aria-label="${side} line ${number}">${escapeHtml(value)}</textarea></label>`;
    }

    function renderMergeEditors(focus = null) {
      if (!comparison.alignedEditors) return;
      comparison.alignedEditors.innerHTML = comparison.mergeRows.map((row, index) => {
        const state = diffEngine.rowState(row);
        const action = state === 'same'
          ? '<i title="Lines match"></i>'
          : `<button type="button" data-local-row-copy="left-to-right" data-row="${index}" title="${row.left === null ? 'Remove this right-side line from the prepared result' : 'Copy this exact left line into the aligned right result'}">${row.left === null ? '× →' : '→'}</button>`;
        return `<div class="local-drive-edit-row ${state}" role="row">${renderMergeCell(row, index, 'left')}<div class="local-drive-row-action">${action}</div>${renderMergeCell(row, index, 'right')}</div>`;
      }).join('');
      renderResultPreview();
      updateComparisonState();
      if (focus) {
        const field = comparison.alignedEditors.querySelector?.(`[data-local-line-side="${focus.side}"][data-row="${focus.row}"]`);
        field?.focus();
        field?.setSelectionRange?.(focus.column, focus.column);
      }
    }

    function updateComparisonState(message = '') {
      const leftDirty = comparisonDirty('left');
      const rightDirty = comparisonDirty('right');
      if (comparison.saveLeft) comparison.saveLeft.disabled = comparison.readOnly || comparison.loading || !leftDirty;
      if (comparison.saveRight) comparison.saveRight.disabled = comparison.readOnly || comparison.loading || !rightDirty;
      if (comparison.copyAllRight) comparison.copyAllRight.disabled = comparison.readOnly || comparison.loading;
      if (comparison.undoResult) comparison.undoResult.disabled = comparison.readOnly || comparison.loading || !comparison.resultHistory.length;
      if (comparison.resetResult) comparison.resetResult.disabled = comparison.readOnly || comparison.loading || !rightDirty;
      if (comparison.previousDifference) comparison.previousDifference.disabled = comparison.loading;
      if (comparison.nextDifference) comparison.nextDifference.disabled = comparison.loading;
      if (!comparison.stateNode) return;
      if (message) comparison.stateNode.textContent = message;
      else if (comparison.readOnly) comparison.stateNode.textContent = 'Folder comparison · read-only';
      else if (leftDirty && rightDirty) comparison.stateNode.textContent = 'Unsaved edits on both sides';
      else if (leftDirty) comparison.stateNode.textContent = 'Unsaved edits on the left';
      else if (rightDirty) comparison.stateNode.textContent = 'Unsaved edits on the right';
      else comparison.stateNode.textContent = 'No unsaved edits';
    }

    function comparisonRules() {
      return comparison.rules?.value || 'exact';
    }

    function renderDiffLine(text, lineNumber, side, same, ignored = false) {
      const kind = text === null ? 'filler' : ignored ? `ignored ${side}` : same ? 'same' : `changed ${side}`;
      const number = lineNumber === null ? '' : lineNumber;
      return `<code class="local-drive-diff-line ${kind}"><i>${number}</i><span>${text === null ? '' : escapeHtml(text) || ' '}</span></code>`;
    }

    function renderComparison() {
      if (!diffEngine || !comparison.diffRows) return;
      const diff = diffEngine.buildLineDiff(comparison.leftText, comparison.rightText, undefined, comparisonRules());
      const previewRows = diff.rows.slice(0, maxDiffPreviewRows);
      comparison.diffRows.innerHTML = previewRows.map((row, rowIndex) => {
        const multiRowBlock = row.hunkFirst && diff.hunks[row.hunkIndex]?.rowCount > 1;
        const lineTitle = row.left === null
          ? 'Remove only this right-side line from the prepared result'
          : row.right === null
            ? 'Insert only this source line into the prepared result'
            : 'Replace only this aligned result line with the source line';
        const mergeButtons = row.same || comparison.readOnly
          ? '<div class="local-drive-hunk-actions"><i title="Lines match under the selected rules"></i></div>'
          : `<div class="local-drive-hunk-actions"><button type="button" class="line-merge ${row.left === null ? 'delete-line' : ''}" data-local-diff-row="${rowIndex}" title="${lineTitle}">${row.left === null ? 'Delete' : '→'}</button>${multiRowBlock ? `<button type="button" class="block-merge" data-local-diff-block="${row.hunkIndex}" title="Replace this complete changed section on the right">Block →</button>` : ''}</div>`;
        return `<div class="local-drive-diff-row ${comparison.activeDifferenceRow === rowIndex ? 'active-difference' : ''}" data-diff-row="${rowIndex}">${renderDiffLine(row.left, row.leftNumber, 'left', row.same, row.ignored)}${mergeButtons}${renderDiffLine(row.right, row.rightNumber, 'right', row.same, row.ignored)}</div>`;
      }).join('') || '<div class="local-drive-diff-empty">Both files are empty and identical.</div>';
      if (diff.rows.length > maxDiffPreviewRows) {
        comparison.diffRows.insertAdjacentHTML('beforeend', `<div class="local-drive-diff-limit">Preview stopped after ${maxDiffPreviewRows.toLocaleString()} aligned rows. Editing and saving still use the complete files.</div>`);
      }
      const qualifier = diff.approximate ? ' · large-file alignment is approximate' : '';
      const encodings = ` · Left ${displayEncoding(comparison.leftEncoding)} / Right ${displayEncoding(comparison.rightEncoding)}`;
      const prefix = comparison.readOnly ? 'Read-only · ' : '';
      comparison.summary.textContent = prefix + (diff.identical
        ? `Identical text · ${diff.rows.length.toLocaleString()} line${diff.rows.length === 1 ? '' : 's'}${encodings}`
        : diff.equivalent
          ? `Equivalent with selected rules · ${diff.ignoredDifferences.toLocaleString()} ignored line difference${diff.ignoredDifferences === 1 ? '' : 's'}${encodings}`
          : `${diff.hunks.length.toLocaleString()} changed block${diff.hunks.length === 1 ? '' : 's'}${diff.ignoredDifferences ? ` · ${diff.ignoredDifferences.toLocaleString()} ignored` : ''}${qualifier}${encodings}`);
      comparison.summary.classList.toggle('identical', diff.equivalent);
      updateComparisonState();
    }

    function setComparisonMode(mode) {
      if (comparison.loading || (comparison.readOnly && mode === 'edit')) return;
      syncComparisonEditors();
      if (mode === 'edit') {
        const rows = diffEngine.createMergeRows(comparison.leftText, comparison.rightText);
        if (rows.length > maxAlignedEditorRows) {
          notify(`This comparison has ${rows.length.toLocaleString()} aligned rows. The interactive editor is limited to ${maxAlignedEditorRows.toLocaleString()} rows to keep the app responsive; block merge and complete-source actions remain available.`, 'error');
          return;
        }
        comparison.mergeRows = rows;
      }
      comparison.mode = mode;
      const editing = mode === 'edit';
      comparison.diffView.hidden = editing;
      comparison.editors.hidden = !editing;
      comparison.showDiff.classList.toggle('active', !editing);
      comparison.showEdit.classList.toggle('active', editing);
      if (editing) {
        renderMergeEditors({ side: 'right', row: 0, column: 0 });
      } else {
        renderComparison();
      }
      updateComparisonState();
    }

    async function openComparisonPair(pair, { returnToFolderMerge = false, readOnly = false } = {}) {
      if (!pair) {
        notify('Select one text file in each pane, or select a file that has the same name in the other pane.', 'error');
        return;
      }
      if (!comparison.dialog || !diffEngine || operationBusy) return;
      const generation = ++comparison.generation;
      operationBusy = true;
      comparison.loading = true;
      comparison.leftPath = pair.left.path || '';
      comparison.rightPath = pair.right.path || '';
      comparison.leftOriginal = comparison.leftText = '';
      comparison.rightOriginal = comparison.rightText = '';
      comparison.leftFingerprint = comparison.rightFingerprint = '';
      comparison.mergeRows = [];
      comparison.resultHistory = [];
      comparison.activeDifferenceRow = -1;
      comparison.readOnly = readOnly;
      comparison.returnToFolderMerge = returnToFolderMerge;
      const leftDisplayPath = pair.left.displayPath || pair.left.path || '(missing on left)';
      const rightDisplayPath = pair.right.displayPath || pair.right.path || '(missing on right)';
      comparison.leftPathNode.textContent = leftDisplayPath;
      comparison.leftPathNode.title = leftDisplayPath;
      comparison.rightPathNode.textContent = rightDisplayPath;
      comparison.rightPathNode.title = rightDisplayPath;
      if (comparison.showEdit) comparison.showEdit.hidden = readOnly;
      for (const node of [comparison.copyAllRight, comparison.undoResult, comparison.resetResult, comparison.saveLeft, comparison.saveRight]) {
        if (node) node.hidden = readOnly;
      }
      comparison.summary.textContent = 'Reading both files…';
      comparison.diffRows.innerHTML = '<div class="local-drive-diff-empty"><i class="spinner"></i> Comparing…</div>';
      comparison.mode = 'diff';
      comparison.diffView.hidden = false;
      comparison.editors.hidden = true;
      comparison.showDiff.classList.add('active');
      comparison.showEdit.classList.remove('active');
      updateComparisonState('Reading both files…');
      comparison.dialog.showModal();
      renderCommandState();
      try {
        const emptySide = { content: '', bytes: 0, encoding: 'utf-8', fingerprint: '' };
        const [leftFile, rightFile] = await Promise.all([
          pair.left.missing ? Promise.resolve(emptySide) : invoke('read_local_text_file', { path: pair.left.path }),
          pair.right.missing ? Promise.resolve(emptySide) : invoke('read_local_text_file', { path: pair.right.path }),
        ]);
        if (generation !== comparison.generation) return;
        comparison.leftOriginal = comparison.leftText = leftFile.content;
        comparison.rightOriginal = comparison.rightText = rightFile.content;
        comparison.leftEncoding = leftFile.encoding || 'utf-8';
        comparison.rightEncoding = rightFile.encoding || 'utf-8';
        comparison.leftFingerprint = leftFile.fingerprint || '';
        comparison.rightFingerprint = rightFile.fingerprint || '';
        comparison.loading = false;
        renderComparison();
      } catch (error) {
        if (generation === comparison.generation) {
          comparison.dialog.close();
          notify(String(error), 'error');
          if (returnToFolderMerge && folderMerge.dialog && !folderMerge.dialog.open) folderMerge.dialog.showModal();
        }
      } finally {
        if (generation === comparison.generation) comparison.loading = false;
        operationBusy = false;
        renderCommandState();
      }
    }

    async function openComparison() {
      return openComparisonPair(comparisonPair());
    }

    function mergeComparisonHunk(direction, hunkIndex) {
      if (comparison.loading || comparison.readOnly) return;
      syncComparisonEditors();
      if (direction === 'left-to-right') rememberRightResult();
      const merged = diffEngine.mergeHunk(comparison.leftText, comparison.rightText, hunkIndex, direction, undefined, comparisonRules());
      comparison.leftText = merged.leftText;
      comparison.rightText = merged.rightText;
      if (comparison.mode === 'edit') rebuildMergeRows();
      renderComparison();
      updateComparisonState('Complete changed block prepared on the right · not saved yet');
    }

    function mergeComparisonRow(rowIndex) {
      if (comparison.loading || comparison.readOnly) return;
      syncComparisonEditors();
      rememberRightResult();
      const merged = diffEngine.mergeRow(comparison.leftText, comparison.rightText, rowIndex, 'left-to-right', undefined, comparisonRules());
      comparison.leftText = merged.leftText;
      comparison.rightText = merged.rightText;
      comparison.activeDifferenceRow = -1;
      renderComparison();
      updateComparisonState('One aligned line prepared on the right · all other lines were preserved · not saved yet');
    }

    function navigateComparisonDifference(direction) {
      if (comparison.loading) return;
      const diff = diffEngine.buildLineDiff(comparison.leftText, comparison.rightText, undefined, comparisonRules());
      const rows = diff.rows.slice(0, maxDiffPreviewRows).map((row, index) => row.same ? -1 : index).filter(index => index >= 0);
      if (!rows.length) {
        comparison.activeDifferenceRow = -1;
        updateComparisonState('No important differences under the selected rules');
        return;
      }
      const current = comparison.activeDifferenceRow;
      if (direction < 0) comparison.activeDifferenceRow = [...rows].reverse().find(index => index < current) ?? rows.at(-1);
      else comparison.activeDifferenceRow = rows.find(index => index > current) ?? rows[0];
      renderComparison();
      comparison.diffRows.querySelector?.(`[data-diff-row="${comparison.activeDifferenceRow}"]`)?.scrollIntoView?.({ block: 'center' });
      updateComparisonState(`Difference ${rows.indexOf(comparison.activeDifferenceRow) + 1} of ${rows.length}`);
    }

    function copyWholeComparison(direction) {
      if (comparison.loading || comparison.readOnly) return;
      syncComparisonEditors();
      if (direction !== 'left-to-right') return;
      rememberRightResult();
      comparison.rightText = comparison.leftText;
      if (comparison.mode === 'edit') rebuildMergeRows();
      if (comparison.mode === 'diff') renderComparison();
      else renderMergeEditors();
      updateComparisonState('Complete source prepared on the right · review, then save explicitly');
    }

    function undoRightResult() {
      syncComparisonEditors();
      const previous = comparison.resultHistory.pop();
      if (previous === undefined) return;
      comparison.rightText = previous;
      if (comparison.mode === 'edit') rebuildMergeRows();
      if (comparison.mode === 'edit') renderMergeEditors();
      else renderComparison();
      updateComparisonState('Last result change undone');
    }

    function resetRightResult() {
      syncComparisonEditors();
      if (comparison.rightText === comparison.rightOriginal) return;
      rememberRightResult();
      comparison.rightText = comparison.rightOriginal;
      if (comparison.mode === 'edit') rebuildMergeRows();
      if (comparison.mode === 'edit') renderMergeEditors();
      else renderComparison();
      updateComparisonState('Result reset to the version currently saved on disk');
    }

    function applyMergeRow(rowIndex) {
      if (comparison.readOnly || comparison.loading || !comparison.mergeRows[rowIndex]) return;
      syncComparisonEditors();
      rememberRightResult();
      comparison.mergeRows = diffEngine.copyMergeRow(comparison.mergeRows, rowIndex, 'left-to-right');
      syncComparisonEditors();
      renderMergeEditors({ side: 'right', row: Math.min(rowIndex, comparison.mergeRows.length - 1), column: 0 });
      updateComparisonState('Source line applied to the prepared right result · not saved yet');
    }

    async function saveComparisonSide(side) {
      syncComparisonEditors();
      if (comparison.readOnly || !comparisonDirty(side) || comparison.loading || operationBusy) return;
      const path = comparison[`${side}Path`];
      operationBusy = true;
      comparison.loading = true;
      updateComparisonState(`Saving ${side} file…`);
      try {
        const result = await invoke('write_local_text_file', {
          path, content: comparison[`${side}Text`], encoding: comparison[`${side}Encoding`],
          expectedFingerprint: comparison[`${side}Fingerprint`] || null,
        });
        comparison[`${side}Original`] = comparison[`${side}Text`];
        comparison[`${side}Fingerprint`] = result?.fingerprint || comparison[`${side}Fingerprint`];
        if (side === 'right') {
          comparison.resultHistory = [];
          if (comparison.returnToFolderMerge) markFolderMergeReviewed(comparison.rightPath, comparison.leftText === comparison.rightText);
        }
        notify(`Saved ${path}`);
        if (sides[side].path) await load(side, sides[side].path, { preserveRows: true });
        options.onMutation?.([path]);
        if (comparison.mode === 'diff') renderComparison();
        else { renderResultPreview(); updateComparisonState(`Saved ${side} file`); }
      } catch (error) {
        notify(String(error), 'error');
        updateComparisonState(`Could not save ${side} file`);
      } finally {
        comparison.loading = false;
        operationBusy = false;
        updateComparisonState();
        renderCommandState();
      }
    }

    async function closeComparison() {
      syncComparisonEditors();
      if ((comparisonDirty('left') || comparisonDirty('right')) && !await ask('Discard the unsaved edits in this comparison?', { title: 'Unsaved comparison edits', danger: true, okLabel: 'Discard' })) return;
      comparison.generation++;
      comparison.dialog.close();
      if (comparison.returnToFolderMerge && folderMerge.dialog && !folderMerge.dialog.open) folderMerge.dialog.showModal();
      comparison.returnToFolderMerge = false;
    }

    function folderRoots() {
      // Folder operations always use the two paths visibly opened in the
      // panes. A merely selected child must never silently change the scope.
      return { left: sides.left.path, right: sides.right.path };
    }

    function selectedFolderMergeEntry() {
      return folderMerge.entries.find(entry => entry.relative_path === folderMerge.selectedPath) || null;
    }

    function folderStatusCopy(entry) {
      if (folderMerge.resolved.has(entry.relative_path)) return entry.reviewResult || 'Reconciled';
      if (folderMerge.skipped.has(entry.relative_path)) return 'Intentionally left different';
      return {
        modified: entry.reviewable ? 'Text differs · open comparison' : 'Different binary content',
        'left-only': entry.item_kind === 'folder' ? 'Folder exists only on left' : 'File exists only on left',
        'right-only': entry.item_kind === 'folder' ? 'Folder exists only on right' : 'File exists only on right',
        'type-conflict': 'File type conflict',
        unsupported: 'Symbolic link · protected',
        'ignored-submodule': 'Git submodule ignored by scan option',
      }[entry.status] || entry.status;
    }

    function folderStatusGlyph(entry) {
      if (folderMerge.resolved.has(entry.relative_path)) return '✓';
      if (folderMerge.skipped.has(entry.relative_path)) return '○';
      return {
        modified: '≠',
        'left-only': '→',
        'right-only': '←',
        'type-conflict': '!',
        unsupported: '!',
        'ignored-submodule': '⑂',
      }[entry.status] || '•';
    }

    function folderSideCell(entry, side) {
      const exists = Boolean(entry[`${side}_path`]);
      const bytes = Number(entry[`${side}_bytes`] || 0);
      const className = (() => {
        if (!exists) return 'missing';
        if (entry.status === 'modified') return 'modified';
        if (entry.status === `${side}-only`) return 'only';
        if (['type-conflict', 'unsupported'].includes(entry.status)) return 'conflict';
        if (entry.status === 'ignored-submodule') return 'ignored';
        return 'same';
      })();
      const detail = exists
        ? `${escapeHtml(entry.item_kind || 'file')}${entry.item_kind === 'file' ? ` · ${formatBytes(bytes)}` : ''}`
        : side === 'left' ? 'missing on left' : 'missing on right';
      const label = exists ? entry.relative_path : '—';
      return `<div class="local-folder-side ${side} ${className}"><span>${side.toUpperCase()}</span><strong>${escapeHtml(label)}</strong><small>${detail}</small></div>`;
    }

    function folderRowActions(entry, compareOnly) {
      if (entry.status === 'ignored-submodule') return '<small>ignored</small>';
      if (entry.item_kind === 'file' && entry.reviewable) {
        return `<button type="button" data-folder-review-entry="${escapeHtml(entry.relative_path)}">${compareOnly ? 'Compare' : 'Open merge'}</button>`;
      }
      if (compareOnly) return `<small>${escapeHtml(folderStatusCopy(entry))}</small>`;
      const leftToRight = canApplyFolderEntry(entry, 'left-to-right')
        ? `<button type="button" data-folder-row-copy="left-to-right" data-folder-merge-entry="${escapeHtml(entry.relative_path)}" title="Copy or replace from left to right">Copy →</button>`
        : '';
      const rightToLeft = canApplyFolderEntry(entry, 'right-to-left')
        ? `<button type="button" data-folder-row-copy="right-to-left" data-folder-merge-entry="${escapeHtml(entry.relative_path)}" title="Copy or replace from right to left">← Copy</button>`
        : '';
      return leftToRight || rightToLeft ? `${leftToRight}${rightToLeft}` : `<small>${escapeHtml(folderStatusCopy(entry))}</small>`;
    }

    function nextFolderMergeEntry() {
      return folderMerge.entries.find(entry => entry.status !== 'ignored-submodule' && !folderMerge.resolved.has(entry.relative_path) && !folderMerge.skipped.has(entry.relative_path)) || null;
    }

    function folderDirection() {
      return folderMerge.direction?.value || 'left-to-right';
    }

    function directionSides(direction = folderDirection()) {
      return direction === 'right-to-left'
        ? { source: 'right', result: 'left', sourceRoot: folderMerge.rightRoot, resultRoot: folderMerge.leftRoot }
        : { source: 'left', result: 'right', sourceRoot: folderMerge.leftRoot, resultRoot: folderMerge.rightRoot };
    }

    function canApplyFolderEntry(entry, direction = folderDirection()) {
      if (!entry || ['ignored-submodule', 'type-conflict', 'unsupported'].includes(entry.status)) return false;
      if (entry.status === 'modified') return entry.item_kind === 'file';
      return (entry.status === 'left-only' && direction === 'left-to-right')
        || (entry.status === 'right-only' && direction === 'right-to-left');
    }

    function renderFolderMerge() {
      if (!folderMerge.dialog) return;
      const compareOnly = folderMerge.mode === 'compare';
      const counts = folderMerge.countsModel || { same: 0, modified: 0, left_only: 0, right_only: 0, conflicts: 0, ignored_submodules: 0 };
      folderMerge.dialog.classList.toggle('compare-mode', compareOnly);
      if (folderMerge.title) folderMerge.title.textContent = compareOnly ? 'Compare folders' : 'Guided folder merge';
      folderMerge.compareMode?.classList.toggle('active', compareOnly);
      folderMerge.guidedMode?.classList.toggle('active', !compareOnly);
      if (folderMerge.directionWrap) folderMerge.directionWrap.hidden = compareOnly;
      if (folderMerge.queueHint) folderMerge.queueHint.textContent = compareOnly ? 'Double-click a text file to open its aligned comparison' : 'Choose a direction; every write is confirmed';
      if (folderMerge.counts) folderMerge.counts.innerHTML = [
        ['same', counts.same, 'Identical omitted'], ['modified', counts.modified, 'Modified'],
        ['left-only', counts.left_only, 'Only on left'], ['right-only', counts.right_only, 'Only on right'],
        ['conflict', counts.conflicts, 'Need manual care'], ['ignored', counts.ignored_submodules, 'Submodules ignored'],
      ].map(([kind, count, label]) => `<span class="local-folder-count ${kind}"><b>${Number(count || 0).toLocaleString()}</b>${label}</span>`).join('');
      const queue = folderMerge.entries;
      if (!folderMerge.selectedPath || !queue.some(entry => entry.relative_path === folderMerge.selectedPath)) {
        folderMerge.selectedPath = nextFolderMergeEntry()?.relative_path || queue[0]?.relative_path || '';
      }
      const visibleQueue = queue.slice(0, folderMerge.visibleLimit);
      if (folderMerge.rows) folderMerge.rows.innerHTML = visibleQueue.map(entry => {
        const selected = entry.relative_path === folderMerge.selectedPath;
        const resolved = folderMerge.resolved.has(entry.relative_path) || folderMerge.skipped.has(entry.relative_path);
        return `<div role="button" tabindex="0" class="local-folder-row ${escapeHtml(entry.status)} ${selected ? 'selected' : ''} ${resolved ? 'resolved' : ''}" data-folder-merge-entry="${escapeHtml(entry.relative_path)}" title="${escapeHtml(entry.relative_path)}">${folderSideCell(entry, 'left')}<div class="local-folder-row-center"><b>${folderStatusGlyph(entry)}</b>${folderRowActions(entry, compareOnly)}</div>${folderSideCell(entry, 'right')}</div>`;
      }).join('') + (queue.length > visibleQueue.length
        ? `<button type="button" class="local-folder-load-more" data-folder-load-more="1">Show next ${Math.min(folderRowPageSize, queue.length - visibleQueue.length).toLocaleString()} differences · ${(queue.length - visibleQueue.length).toLocaleString()} remaining</button>`
        : '') || '<div class="local-drive-empty">The two folder structures are identical.</div>';
      const pending = queue.filter(entry => entry.status !== 'ignored-submodule' && !folderMerge.resolved.has(entry.relative_path) && !folderMerge.skipped.has(entry.relative_path)).length;
      if (folderMerge.summary) folderMerge.summary.textContent = folderMerge.loading
        ? 'Scanning both folder structures without changing them…'
        : compareOnly
          ? `${queue.length.toLocaleString()} difference${queue.length === 1 ? '' : 's'} · ${Number(counts.same || 0).toLocaleString()} identical item${counts.same === 1 ? '' : 's'} omitted`
          : `${pending.toLocaleString()} difference${pending === 1 ? '' : 's'} left to reconcile · no automatic deletion`;
      const selected = selectedFolderMergeEntry();
      const finished = selected && (folderMerge.resolved.has(selected.relative_path) || folderMerge.skipped.has(selected.relative_path));
      if (folderMerge.review) {
        folderMerge.review.hidden = false;
        folderMerge.review.disabled = folderMerge.loading || finished || selected?.item_kind !== 'file' || !selected?.reviewable;
        folderMerge.review.textContent = compareOnly ? 'Open file comparison…' : `Open ${folderDirection() === 'left-to-right' ? 'left → right' : 'right → left'} merge…`;
      }
      if (folderMerge.skip) {
        folderMerge.skip.hidden = compareOnly;
        folderMerge.skip.disabled = folderMerge.loading || !selected || finished || selected?.status === 'ignored-submodule';
      }
      if (folderMerge.apply) {
        folderMerge.apply.hidden = compareOnly;
        folderMerge.apply.disabled = folderMerge.loading || finished || !canApplyFolderEntry(selected);
        const direction = folderDirection();
        const item = selected?.item_kind === 'folder' ? 'folder' : 'file';
        folderMerge.apply.textContent = selected?.status === 'modified'
          ? `Use ${direction === 'left-to-right' ? 'left → right' : 'right → left'}`
          : `Copy ${item} ${direction === 'left-to-right' ? '→' : '←'}`;
      }
      if (folderMerge.rescan) folderMerge.rescan.disabled = folderMerge.loading;
      if (folderMerge.compareMode) folderMerge.compareMode.disabled = folderMerge.loading;
      if (folderMerge.guidedMode) folderMerge.guidedMode.disabled = folderMerge.loading;
      if (folderMerge.direction) folderMerge.direction.disabled = folderMerge.loading;
      if (folderMerge.ignoreSubmodules) folderMerge.ignoreSubmodules.disabled = folderMerge.loading;
      if (folderMerge.stateNode && !folderMerge.loading) folderMerge.stateNode.textContent = compareOnly
        ? 'Read-only comparison · selecting a row never changes either folder.'
        : pending ? 'Every copy or replacement requires an explicit confirmation.' : 'Review complete. Rescan to verify both structures.';
    }

    function renderFolderPreview() {
      const entry = selectedFolderMergeEntry();
      if (!folderMerge.previewContent) return;
      if (!entry) {
        if (folderMerge.previewTitle) folderMerge.previewTitle.textContent = 'No difference selected';
        if (folderMerge.previewStatus) folderMerge.previewStatus.textContent = 'The scan found no actionable item';
        folderMerge.previewContent.innerHTML = '<p>The two opened folders are identical for the selected scan options.</p>';
        return;
      }
      folderMerge.previewTitle.textContent = entry.relative_path;
      folderMerge.previewStatus.textContent = folderStatusCopy(entry);
      const explanation = entry.status === 'ignored-submodule'
        ? 'This entire nested Git repository is excluded. Its files are not scanned and no merge action can enter it.'
        : entry.item_kind === 'folder'
          ? 'This directory exists on only one side. Guided merge can create the matching directory on the other side; it never deletes a directory automatically.'
          : entry.reviewable
            ? folderMerge.mode === 'compare' ? 'Double-click to open the full aligned file comparison. Modified pairs can be edited and saved explicitly; one-sided files stay read-only.' : 'Open the aligned merge for line-level control, or copy the complete selected source after confirmation.'
            : 'Content cannot be shown in the text viewer. Guided merge can copy the complete regular file only after confirmation.';
      folderMerge.previewContent.innerHTML = `<div class="local-folder-item-summary"><div><span>LEFT</span><code>${escapeHtml(entry.left_path || 'Missing')}</code></div><div><span>RIGHT</span><code>${escapeHtml(entry.right_path || 'Missing')}</code></div><p>${escapeHtml(explanation)}</p></div>`;
    }

    async function scanFolderMerge() {
      if (!folderMerge.leftRoot || !folderMerge.rightRoot || folderMerge.loading) return;
      const generation = ++folderMerge.generation;
      folderMerge.loading = true;
      folderMerge.stateNode.textContent = 'Scanning both folders…';
      renderFolderMerge();
      try {
        const result = await invoke('compare_local_directories', {
          leftPath: folderMerge.leftRoot,
          rightPath: folderMerge.rightRoot,
          ignoreSubmodules: folderMerge.ignoreSubmodules?.checked !== false,
        });
        if (generation !== folderMerge.generation) return;
        folderMerge.leftRoot = result.left_root;
        folderMerge.rightRoot = result.right_root;
        folderMerge.entries = result.entries || [];
        folderMerge.countsModel = result.counts || {};
        folderMerge.resolved.clear();
        folderMerge.skipped.clear();
        folderMerge.visibleLimit = folderRowPageSize;
        folderMerge.selectedPath = folderMerge.entries.find(entry => entry.status !== 'ignored-submodule')?.relative_path || folderMerge.entries[0]?.relative_path || '';
        folderMerge.leftPathNode.textContent = folderMerge.leftRoot;
        folderMerge.leftPathNode.title = folderMerge.leftRoot;
        folderMerge.rightPathNode.textContent = folderMerge.rightRoot;
        folderMerge.rightPathNode.title = folderMerge.rightRoot;
      } catch (error) {
        if (generation === folderMerge.generation) {
          notify(String(error), 'error');
          folderMerge.dialog.close();
        }
      } finally {
        if (generation === folderMerge.generation) {
          folderMerge.loading = false;
          renderFolderMerge();
          renderFolderPreview();
        }
      }
    }

    async function openFolderOperation(mode = 'compare') {
      if (!folderMerge.dialog || operationBusy || sides.left.loading || sides.right.loading) return;
      const roots = folderRoots();
      if (!roots.left || !roots.right) return notify('Choose one folder on the left and one folder on the right.', 'error');
      if (roots.left === roots.right) return notify('Choose two different folders to compare.', 'error');
      folderMerge.mode = mode;
      folderMerge.leftRoot = roots.left;
      folderMerge.rightRoot = roots.right;
      folderMerge.entries = [];
      folderMerge.selectedPath = '';
      folderMerge.leftPathNode.textContent = roots.left;
      folderMerge.rightPathNode.textContent = roots.right;
      folderMerge.dialog.showModal();
      await scanFolderMerge();
    }

    function advanceFolderMerge() {
      const next = nextFolderMergeEntry();
      folderMerge.selectedPath = next?.relative_path || folderMerge.selectedPath;
      const nextIndex = next ? folderMerge.entries.indexOf(next) : -1;
      if (nextIndex >= folderMerge.visibleLimit) folderMerge.visibleLimit = nextIndex + 1;
      renderFolderMerge();
      renderFolderPreview();
    }

    function markFolderMergeReviewed(resultPath, identicalWithSource) {
      const entry = folderMerge.entries.find(item => item.left_path === resultPath || item.right_path === resultPath);
      if (!entry) return;
      folderMerge.resolved.add(entry.relative_path);
      entry.reviewResult = identicalWithSource ? 'Matches selected source' : 'Custom merged result saved';
      advanceFolderMerge();
    }

    async function applyFolderMergeEntry(entry = selectedFolderMergeEntry(), direction = folderDirection()) {
      if (!entry || folderMerge.mode !== 'merge' || folderMerge.loading || operationBusy || !canApplyFolderEntry(entry, direction)) return;
      const sidesForDirection = directionSides(direction);
      const sourcePath = entry[`${sidesForDirection.source}_path`];
      const resultPath = entry[`${sidesForDirection.result}_path`] || nativeChildPath(sidesForDirection.resultRoot, entry.relative_path);
      const isNew = entry.status !== 'modified';
      const action = isNew
        ? `create this ${entry.item_kind} on the ${sidesForDirection.result}`
        : `replace the complete ${sidesForDirection.result} file with the ${sidesForDirection.source} file`;
      const warning = entry.status === 'modified' ? '\n\nFor line-by-line control, choose “Open aligned merge” instead.' : '';
      if (!await ask(`SOURCE · ${sidesForDirection.source.toUpperCase()}\n${sourcePath}\n\nRESULT · ${sidesForDirection.result.toUpperCase()}\n${resultPath}\n\nThis will ${action}. Nothing is deleted.${warning}`, {
        title: isNew ? `Copy ${entry.item_kind} ${direction === 'left-to-right' ? 'left → right' : 'right → left'}` : `Replace ${sidesForDirection.result} file`,
        okLabel: isNew ? `Copy ${entry.item_kind}` : `Replace ${sidesForDirection.result} file`,
      })) return;
      operationBusy = true;
      folderMerge.loading = true;
      renderFolderMerge();
      try {
        const command = entry.item_kind === 'folder'
          ? 'create_local_merge_directory'
          : isNew ? 'copy_local_merge_file' : 'replace_local_merge_file';
        const args = { leftRoot: sidesForDirection.sourceRoot, rightRoot: sidesForDirection.resultRoot, relativePath: entry.relative_path };
        if (!isNew) args.expectedRightFingerprint = entry[`${sidesForDirection.result}_fingerprint`];
        const result = await invoke(command, args);
        entry[`${sidesForDirection.result}_path`] = result.destination;
        folderMerge.resolved.add(entry.relative_path);
        entry.reviewResult = `${isNew ? 'Copied' : 'Updated'} ${sidesForDirection.source} → ${sidesForDirection.result}`;
        notify(`${entry.reviewResult}: ${entry.relative_path}`);
        options.onMutation?.([result.destination]);
      } catch (error) {
        notify(String(error), 'error');
      } finally {
        folderMerge.loading = false;
        advanceFolderMerge();
        if (sides[sidesForDirection.result].path) load(sidesForDirection.result, sides[sidesForDirection.result].path, { preserveRows: true });
      }
    }

    function skipFolderMergeEntry() {
      const entry = selectedFolderMergeEntry();
      if (!entry || folderMerge.loading) return;
      folderMerge.skipped.add(entry.relative_path);
      advanceFolderMerge();
    }

    async function reviewFolderMergeEntry() {
      const entry = selectedFolderMergeEntry();
      if (!entry?.reviewable || entry.item_kind !== 'file') return;
      const compareOnly = folderMerge.mode === 'compare';
      const direction = compareOnly ? 'left-to-right' : folderDirection();
      const ordered = directionSides(direction);
      const sourcePath = entry[`${ordered.source}_path`];
      const resultPath = entry[`${ordered.result}_path`];
      if (!compareOnly && (!sourcePath || !resultPath || entry.status !== 'modified')) return;
      folderMerge.dialog.close();
      await openComparisonPair({
        left: sourcePath
          ? { path: sourcePath, name: entry.relative_path }
          : { missing: true, displayPath: `Missing · ${nativeChildPath(ordered.sourceRoot, entry.relative_path)}`, name: entry.relative_path },
        right: resultPath
          ? { path: resultPath, name: entry.relative_path }
          : { missing: true, displayPath: `Missing · ${nativeChildPath(ordered.resultRoot, entry.relative_path)}`, name: entry.relative_path },
      }, { returnToFolderMerge: true, readOnly: compareOnly && entry.status !== 'modified' });
    }

    function closeFolderMerge() {
      folderMerge.generation++;
      folderMerge.previewGeneration++;
      folderMerge.loading = false;
      folderMerge.dialog?.close();
    }

    function runShortcut(action) {
      if (action === 'compare') openComparison();
      else if (action === 'view') openTextFile(false);
      else if (action === 'edit') openTextFile(true);
      else if (action === 'copy') copySelected();
      else if (action === 'move') moveSelected();
      else if (action === 'mkdir') createFolder();
      else if (action === 'trash') moveSelectedToTrash();
      else if (action === 'folder-compare') openFolderOperation('compare');
      else if (action === 'folder-merge') openFolderOperation('merge');
    }

    root?.addEventListener('click', event => {
      const pane = event.target.closest('[data-drive-side]');
      const side = pane?.dataset.driveSide;
      if (side) { activeSide = side; renderActivePane(); }
      const entryButton = event.target.closest('[data-drive-entry]');
      if (side && entryButton) {
        const entry = sides[side].entries[Number(entryButton.dataset.driveEntry)];
        sides[side].selectedPath = entry?.path || '';
        renderSelection(side);
        return;
      }
      const action = event.target.closest('[data-drive-action]')?.dataset.driveAction;
      if (side && action === 'choose') choose(side);
      else if (side && action === 'refresh' && sides[side].path) load(side, sides[side].path);
      const shortcut = event.target.closest('[data-drive-shortcut]')?.dataset.driveShortcut;
      if (shortcut) runShortcut(shortcut);
    });

    root?.addEventListener('dblclick', event => {
      const pane = event.target.closest('[data-drive-side]');
      if (!pane) return;
      const side = pane.dataset.driveSide;
      activeSide = side;
      if (event.target.closest('[data-drive-up]')) {
        if (sides[side].parent) load(side, sides[side].parent);
        return;
      }
      const button = event.target.closest('[data-drive-entry]');
      if (!button) return;
      const entry = sides[side].entries[Number(button.dataset.driveEntry)];
      if (entry?.kind === 'folder') load(side, entry.path);
      else if (entry?.kind === 'file') openTextFile(false);
    });

    documentRef?.addEventListener('keydown', event => {
      if (!active || root?.hidden || editor.dialog?.open || comparison.dialog?.open || folderMerge.dialog?.open || ['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target?.tagName)) return;
      const shortcuts = { F2: 'compare', F3: 'view', F4: 'edit', F5: 'copy', F6: 'move', F7: 'mkdir', F8: 'trash', F9: 'folder-compare', F10: 'folder-merge' };
      if (shortcuts[event.key]) {
        event.preventDefault();
        runShortcut(shortcuts[event.key]);
        return;
      }
      if (event.key === 'Backspace' && sides[activeSide].parent) {
        event.preventDefault();
        load(activeSide, sides[activeSide].parent);
      } else if (event.key === 'Enter') {
        const entry = selectedEntry();
        if (entry?.kind === 'folder') { event.preventDefault(); load(activeSide, entry.path); }
        else if (entry?.kind === 'file') { event.preventDefault(); openTextFile(false); }
      }
    });

    editor.content?.addEventListener('input', () => {
      if (!editor.editing) return;
      const changed = editor.content.value !== editor.original;
      editor.save.disabled = !changed;
      editor.state.textContent = changed ? 'Unsaved local edits' : 'Saved locally · no changes';
    });
    editor.save?.addEventListener('click', saveTextFile);
    editor.close?.addEventListener('click', closeTextEditor);
    editor.cancel?.addEventListener('click', closeTextEditor);
    editor.dialog?.addEventListener('cancel', event => { event.preventDefault(); closeTextEditor(); });
    comparison.showDiff?.addEventListener('click', () => setComparisonMode('diff'));
    comparison.showEdit?.addEventListener('click', () => setComparisonMode('edit'));
    comparison.previousDifference?.addEventListener('click', () => navigateComparisonDifference(-1));
    comparison.nextDifference?.addEventListener('click', () => navigateComparisonDifference(1));
    comparison.rules?.addEventListener('change', () => {
      comparison.activeDifferenceRow = -1;
      if (comparison.mode === 'diff') renderComparison();
    });
    comparison.copyAllRight?.addEventListener('click', () => copyWholeComparison('left-to-right'));
    comparison.undoResult?.addEventListener('click', undoRightResult);
    comparison.resetResult?.addEventListener('click', resetRightResult);
    comparison.saveLeft?.addEventListener('click', () => saveComparisonSide('left'));
    comparison.saveRight?.addEventListener('click', () => saveComparisonSide('right'));
    comparison.close?.addEventListener('click', closeComparison);
    comparison.cancel?.addEventListener('click', closeComparison);
    comparison.dialog?.addEventListener('cancel', event => { event.preventDefault(); closeComparison(); });
    comparison.diffRows?.addEventListener('click', event => {
      const line = event.target.closest('[data-local-diff-row]');
      if (line) return mergeComparisonRow(Number(line.dataset.localDiffRow));
      const block = event.target.closest('[data-local-diff-block]');
      if (block) mergeComparisonHunk('left-to-right', Number(block.dataset.localDiffBlock));
    });
    comparison.alignedEditors?.addEventListener('click', event => {
      if (comparison.readOnly) return;
      const copy = event.target.closest('[data-local-row-copy]');
      if (copy) return applyMergeRow(Number(copy.dataset.row));
      const createLine = event.target.closest('[data-local-line-create]');
      if (!createLine) return;
      const side = createLine.dataset.localLineCreate;
      const row = Number(createLine.dataset.row);
      if (side === 'right') rememberRightResult();
      comparison.mergeRows = diffEngine.setMergeLine(comparison.mergeRows, row, side, '');
      syncComparisonEditors();
      renderMergeEditors({ side, row, column: 0 });
    });
    comparison.alignedEditors?.addEventListener('focusin', event => {
      const field = event.target.closest('[data-local-line-side]');
      if (!field || field.dataset.localLineSide !== 'right') return;
      field.dataset.resultHistoryCaptured = 'false';
    });
    comparison.alignedEditors?.addEventListener('input', event => {
      if (comparison.readOnly) return;
      const field = event.target.closest('[data-local-line-side]');
      if (!field) return;
      const side = field.dataset.localLineSide;
      const row = Number(field.dataset.row);
      if (side === 'right' && field.dataset.resultHistoryCaptured !== 'true') {
        rememberRightResult();
        field.dataset.resultHistoryCaptured = 'true';
      }
      const multiline = /[\r\n]/.test(field.value);
      comparison.mergeRows = diffEngine.setMergeLine(comparison.mergeRows, row, side, field.value);
      syncComparisonEditors();
      if (multiline) renderMergeEditors({ side, row, column: 0 });
      else { renderResultPreview(); updateComparisonState(); }
    });
    comparison.alignedEditors?.addEventListener('keydown', event => {
      if (comparison.readOnly) return;
      const field = event.target.closest('[data-local-line-side]');
      if (!field) return;
      const side = field.dataset.localLineSide;
      const row = Number(field.dataset.row);
      if (event.key === 'Enter') {
        event.preventDefault();
        if (side === 'right' && field.dataset.resultHistoryCaptured !== 'true') rememberRightResult();
        const inserted = diffEngine.insertMergeLine(comparison.mergeRows, row, side, field.selectionStart || 0);
        comparison.mergeRows = inserted.rows;
        syncComparisonEditors();
        renderMergeEditors({ side, row: inserted.focusRow, column: inserted.focusColumn });
      } else if (event.key === 'Backspace' && field.selectionStart === 0 && field.selectionEnd === 0) {
        const joined = diffEngine.joinMergeLineBackward(comparison.mergeRows, row, side);
        if (joined.focusRow === row) return;
        event.preventDefault();
        if (side === 'right' && field.dataset.resultHistoryCaptured !== 'true') rememberRightResult();
        comparison.mergeRows = joined.rows;
        syncComparisonEditors();
        renderMergeEditors({ side, row: joined.focusRow, column: joined.focusColumn });
      }
    });
    folderMerge.rows?.addEventListener('click', event => {
      if (event.target.closest('[data-folder-load-more]')) {
        folderMerge.visibleLimit += folderRowPageSize;
        renderFolderMerge();
        return;
      }
      const rowCopy = event.target.closest('[data-folder-row-copy]');
      if (rowCopy) {
        folderMerge.selectedPath = rowCopy.dataset.folderMergeEntry;
        const entry = selectedFolderMergeEntry();
        if (entry && folderMerge.direction) folderMerge.direction.value = rowCopy.dataset.folderRowCopy;
        renderFolderMerge();
        renderFolderPreview();
        applyFolderMergeEntry(entry, rowCopy.dataset.folderRowCopy);
        return;
      }
      const reviewButton = event.target.closest('[data-folder-review-entry]');
      if (reviewButton) {
        folderMerge.selectedPath = reviewButton.dataset.folderReviewEntry;
        renderFolderMerge();
        renderFolderPreview();
        reviewFolderMergeEntry();
        return;
      }
      const row = event.target.closest('[data-folder-merge-entry]');
      if (!row) return;
      const now = Date.now();
      const isDoubleClick = lastFolderMergeClick.path === row.dataset.folderMergeEntry && now - lastFolderMergeClick.time < 750;
      lastFolderMergeClick = { path: row.dataset.folderMergeEntry, time: now };
      folderMerge.selectedPath = row.dataset.folderMergeEntry;
      const entry = selectedFolderMergeEntry();
      if (folderMerge.mode === 'merge' && folderMerge.direction) {
        if (entry?.status === 'left-only') folderMerge.direction.value = 'left-to-right';
        else if (entry?.status === 'right-only') folderMerge.direction.value = 'right-to-left';
      }
      renderFolderMerge();
      renderFolderPreview();
      if (isDoubleClick && entry?.item_kind === 'file' && entry.reviewable) {
        lastFolderMergeClick = { path: '', time: 0 };
        reviewFolderMergeEntry();
      }
    });
    folderMerge.rows?.addEventListener('dblclick', event => {
      const row = event.target.closest('[data-folder-merge-entry]');
      if (!row) return;
      folderMerge.selectedPath = row.dataset.folderMergeEntry;
      const entry = selectedFolderMergeEntry();
      if (entry?.item_kind === 'file' && entry.reviewable) reviewFolderMergeEntry();
    });
    folderMerge.compareMode?.addEventListener('click', () => { folderMerge.mode = 'compare'; renderFolderMerge(); renderFolderPreview(); });
    folderMerge.guidedMode?.addEventListener('click', () => { folderMerge.mode = 'merge'; renderFolderMerge(); renderFolderPreview(); });
    folderMerge.direction?.addEventListener('change', () => { renderFolderMerge(); renderFolderPreview(); });
    folderMerge.ignoreSubmodules?.addEventListener('change', scanFolderMerge);
    folderMerge.rescan?.addEventListener('click', scanFolderMerge);
    folderMerge.skip?.addEventListener('click', skipFolderMergeEntry);
    folderMerge.review?.addEventListener('click', reviewFolderMergeEntry);
    folderMerge.apply?.addEventListener('click', applyFolderMergeEntry);
    folderMerge.close?.addEventListener('click', closeFolderMerge);
    folderMerge.cancel?.addEventListener('click', closeFolderMerge);
    folderMerge.dialog?.addEventListener('cancel', event => { event.preventDefault(); closeFolderMerge(); });

    return {
      async activate(repositoryPath) {
        active = true;
        if (repositoryPath && initializedRepository !== repositoryPath) {
          initializedRepository = repositoryPath;
          activeSide = 'right';
          const right = await load('right', repositoryPath);
          const leftStart = right?.parent || nativeParentPath(repositoryPath);
          if (leftStart) await load('left', leftStart);
        }
        render();
      },
      openRepositoryLocation,
      deactivate() { active = false; },
      setFilter(value) { filter = value || ''; render(); },
      reload() {
        for (const side of ['left', 'right']) if (sides[side].path) load(side, sides[side].path);
      },
    };
  }

  const api = { create, displayEncoding, formatBytes, nativeChildPath, nativeParentPath };
  global.LocalDriveWorkspace = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
