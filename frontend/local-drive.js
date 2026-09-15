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
      filePath: '', original: '', encoding: 'utf-8', side: 'right', editing: false,
    };
    const comparison = {
      dialog: documentRef?.querySelector('#localDriveCompareDialog'),
      summary: documentRef?.querySelector('#localDriveCompareSummary'),
      leftPathNode: documentRef?.querySelector('#localDriveCompareLeftPath'),
      rightPathNode: documentRef?.querySelector('#localDriveCompareRightPath'),
      diffView: documentRef?.querySelector('#localDriveDiffView'),
      diffRows: documentRef?.querySelector('#localDriveDiffRows'),
      editors: documentRef?.querySelector('#localDriveMergeEditors'),
      leftContent: documentRef?.querySelector('#localDriveCompareLeftContent'),
      rightContent: documentRef?.querySelector('#localDriveCompareRightContent'),
      showDiff: documentRef?.querySelector('#showLocalDriveDiff'),
      showEdit: documentRef?.querySelector('#editLocalDriveDiff'),
      copyAllRight: documentRef?.querySelector('#copyAllLocalDriveRight'),
      copyAllLeft: documentRef?.querySelector('#copyAllLocalDriveLeft'),
      saveLeft: documentRef?.querySelector('#saveLocalDriveCompareLeft'),
      saveRight: documentRef?.querySelector('#saveLocalDriveCompareRight'),
      stateNode: documentRef?.querySelector('#localDriveCompareState'),
      close: documentRef?.querySelector('#closeLocalDriveCompare'),
      cancel: documentRef?.querySelector('#cancelLocalDriveCompare'),
      leftPath: '', rightPath: '', leftOriginal: '', rightOriginal: '',
      leftText: '', rightText: '', leftEncoding: 'utf-8', rightEncoding: 'utf-8', mode: 'diff', generation: 0, loading: false,
    };
    let active = false;
    let initializedRepository = '';
    let activeSide = 'right';
    let filter = '';
    let operationBusy = false;

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
        await invoke('write_local_text_file', { path, content: editor.content.value, encoding: editor.encoding });
        editor.original = editor.content.value;
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
      if (comparison.mode !== 'edit') return;
      comparison.leftText = comparison.leftContent?.value ?? comparison.leftText;
      comparison.rightText = comparison.rightContent?.value ?? comparison.rightText;
    }

    function updateComparisonState(message = '') {
      const leftDirty = comparisonDirty('left');
      const rightDirty = comparisonDirty('right');
      if (comparison.saveLeft) comparison.saveLeft.disabled = comparison.loading || !leftDirty;
      if (comparison.saveRight) comparison.saveRight.disabled = comparison.loading || !rightDirty;
      if (comparison.copyAllRight) comparison.copyAllRight.disabled = comparison.loading;
      if (comparison.copyAllLeft) comparison.copyAllLeft.disabled = comparison.loading;
      if (!comparison.stateNode) return;
      if (message) comparison.stateNode.textContent = message;
      else if (leftDirty && rightDirty) comparison.stateNode.textContent = 'Unsaved edits on both sides';
      else if (leftDirty) comparison.stateNode.textContent = 'Unsaved edits on the left';
      else if (rightDirty) comparison.stateNode.textContent = 'Unsaved edits on the right';
      else comparison.stateNode.textContent = 'No unsaved edits';
    }

    function renderDiffLine(text, lineNumber, side, same) {
      const kind = text === null ? 'filler' : same ? 'same' : `changed ${side}`;
      const number = lineNumber === null ? '' : lineNumber;
      return `<code class="local-drive-diff-line ${kind}"><i>${number}</i><span>${text === null ? '' : escapeHtml(text) || ' '}</span></code>`;
    }

    function renderComparison() {
      if (!diffEngine || !comparison.diffRows) return;
      const diff = diffEngine.buildLineDiff(comparison.leftText, comparison.rightText);
      const maxPreviewRows = 5000;
      const previewRows = diff.rows.slice(0, maxPreviewRows);
      comparison.diffRows.innerHTML = previewRows.map(row => {
        const mergeButtons = row.hunkFirst
          ? `<div class="local-drive-hunk-actions"><button type="button" data-local-diff-merge="left-to-right" data-hunk="${row.hunkIndex}" title="Use this change from the left file in the right file">→</button><button type="button" data-local-diff-merge="right-to-left" data-hunk="${row.hunkIndex}" title="Use this change from the right file in the left file">←</button></div>`
          : '<span></span>';
        return `<div class="local-drive-diff-row">${renderDiffLine(row.left, row.leftNumber, 'left', row.same)}${mergeButtons}${renderDiffLine(row.right, row.rightNumber, 'right', row.same)}</div>`;
      }).join('') || '<div class="local-drive-diff-empty">Both files are empty and identical.</div>';
      if (diff.rows.length > maxPreviewRows) {
        comparison.diffRows.insertAdjacentHTML('beforeend', `<div class="local-drive-diff-limit">Preview stopped after ${maxPreviewRows.toLocaleString()} aligned rows. Editing and saving still use the complete files.</div>`);
      }
      const qualifier = diff.approximate ? ' · large-file alignment is approximate' : '';
      const encodings = ` · Left ${displayEncoding(comparison.leftEncoding)} / Right ${displayEncoding(comparison.rightEncoding)}`;
      comparison.summary.textContent = diff.identical
        ? `Identical text · ${diff.rows.length.toLocaleString()} line${diff.rows.length === 1 ? '' : 's'}${encodings}`
        : `${diff.hunks.length.toLocaleString()} changed block${diff.hunks.length === 1 ? '' : 's'}${qualifier}${encodings}`;
      comparison.summary.classList.toggle('identical', diff.identical);
      updateComparisonState();
    }

    function setComparisonMode(mode) {
      if (comparison.loading) return;
      syncComparisonEditors();
      comparison.mode = mode;
      const editing = mode === 'edit';
      comparison.diffView.hidden = editing;
      comparison.editors.hidden = !editing;
      comparison.showDiff.classList.toggle('active', !editing);
      comparison.showEdit.classList.toggle('active', editing);
      if (editing) {
        comparison.leftContent.value = comparison.leftText;
        comparison.rightContent.value = comparison.rightText;
        comparison.leftContent.focus();
      } else {
        renderComparison();
      }
      updateComparisonState();
    }

    async function openComparison() {
      const pair = comparisonPair();
      if (!pair) {
        notify('Select one text file in each pane, or select a file that has the same name in the other pane.', 'error');
        return;
      }
      if (!comparison.dialog || !diffEngine || operationBusy) return;
      const generation = ++comparison.generation;
      operationBusy = true;
      comparison.loading = true;
      comparison.leftPath = pair.left.path;
      comparison.rightPath = pair.right.path;
      comparison.leftOriginal = comparison.leftText = '';
      comparison.rightOriginal = comparison.rightText = '';
      comparison.leftPathNode.textContent = pair.left.path;
      comparison.leftPathNode.title = pair.left.path;
      comparison.rightPathNode.textContent = pair.right.path;
      comparison.rightPathNode.title = pair.right.path;
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
        const [leftFile, rightFile] = await Promise.all([
          invoke('read_local_text_file', { path: pair.left.path }),
          invoke('read_local_text_file', { path: pair.right.path }),
        ]);
        if (generation !== comparison.generation) return;
        comparison.leftOriginal = comparison.leftText = leftFile.content;
        comparison.rightOriginal = comparison.rightText = rightFile.content;
        comparison.leftEncoding = leftFile.encoding || 'utf-8';
        comparison.rightEncoding = rightFile.encoding || 'utf-8';
        comparison.leftContent.value = leftFile.content;
        comparison.rightContent.value = rightFile.content;
        comparison.loading = false;
        renderComparison();
      } catch (error) {
        if (generation === comparison.generation) {
          comparison.dialog.close();
          notify(String(error), 'error');
        }
      } finally {
        if (generation === comparison.generation) comparison.loading = false;
        operationBusy = false;
        renderCommandState();
      }
    }

    function mergeComparisonHunk(direction, hunkIndex) {
      if (comparison.loading) return;
      syncComparisonEditors();
      const merged = diffEngine.mergeHunk(comparison.leftText, comparison.rightText, hunkIndex, direction);
      comparison.leftText = merged.leftText;
      comparison.rightText = merged.rightText;
      comparison.leftContent.value = merged.leftText;
      comparison.rightContent.value = merged.rightText;
      renderComparison();
    }

    function copyWholeComparison(direction) {
      if (comparison.loading) return;
      syncComparisonEditors();
      if (direction === 'left-to-right') comparison.rightText = comparison.leftText;
      else comparison.leftText = comparison.rightText;
      comparison.leftContent.value = comparison.leftText;
      comparison.rightContent.value = comparison.rightText;
      if (comparison.mode === 'diff') renderComparison();
      else updateComparisonState(direction === 'left-to-right' ? 'Left content copied to the right editor · save explicitly' : 'Right content copied to the left editor · save explicitly');
    }

    async function saveComparisonSide(side) {
      syncComparisonEditors();
      if (!comparisonDirty(side) || comparison.loading || operationBusy) return;
      const path = comparison[`${side}Path`];
      operationBusy = true;
      comparison.loading = true;
      updateComparisonState(`Saving ${side} file…`);
      try {
        await invoke('write_local_text_file', { path, content: comparison[`${side}Text`], encoding: comparison[`${side}Encoding`] });
        comparison[`${side}Original`] = comparison[`${side}Text`];
        notify(`Saved ${path}`);
        if (sides[side].path) await load(side, sides[side].path, { preserveRows: true });
        options.onMutation?.([path]);
        if (comparison.mode === 'diff') renderComparison();
        else updateComparisonState(`Saved ${side} file`);
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
    }

    function runShortcut(action) {
      if (action === 'compare') openComparison();
      else if (action === 'view') openTextFile(false);
      else if (action === 'edit') openTextFile(true);
      else if (action === 'copy') copySelected();
      else if (action === 'move') moveSelected();
      else if (action === 'mkdir') createFolder();
      else if (action === 'trash') moveSelectedToTrash();
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
      if (!active || root?.hidden || editor.dialog?.open || comparison.dialog?.open || ['INPUT', 'TEXTAREA', 'SELECT'].includes(event.target?.tagName)) return;
      const shortcuts = { F2: 'compare', F3: 'view', F4: 'edit', F5: 'copy', F6: 'move', F7: 'mkdir', F8: 'trash' };
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
    comparison.copyAllRight?.addEventListener('click', () => copyWholeComparison('left-to-right'));
    comparison.copyAllLeft?.addEventListener('click', () => copyWholeComparison('right-to-left'));
    comparison.saveLeft?.addEventListener('click', () => saveComparisonSide('left'));
    comparison.saveRight?.addEventListener('click', () => saveComparisonSide('right'));
    comparison.close?.addEventListener('click', closeComparison);
    comparison.cancel?.addEventListener('click', closeComparison);
    comparison.dialog?.addEventListener('cancel', event => { event.preventDefault(); closeComparison(); });
    comparison.diffRows?.addEventListener('click', event => {
      const button = event.target.closest('[data-local-diff-merge]');
      if (!button) return;
      mergeComparisonHunk(button.dataset.localDiffMerge, Number(button.dataset.hunk));
    });
    for (const content of [comparison.leftContent, comparison.rightContent]) {
      content?.addEventListener('input', () => {
        syncComparisonEditors();
        updateComparisonState();
      });
    }

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
      deactivate() { active = false; },
      setFilter(value) { filter = value || ''; render(); },
      reload() {
        for (const side of ['left', 'right']) if (sides[side].path) load(side, sides[side].path);
      },
    };
  }

  const api = { create, displayEncoding, formatBytes, nativeParentPath };
  global.LocalDriveWorkspace = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
