(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  root.SubmoduleStateModel = api;
})(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  const states = {
    changes_inside: {
      row: 'Changes inside',
      short: 'Changes inside · commit needed',
      detail: 'Files inside this submodule have not been committed yet. Commit them in the submodule first.',
      tone: 'changed', actionable: true,
    },
    local_commit_push_needed: {
      row: 'Local commit',
      short: 'Local commit · push needed',
      detail: 'The submodule commit exists locally but is not on origin yet. Push the submodule before recording it in the project.',
      tone: 'unpushed', actionable: true,
    },
    sync_needed: {
      row: 'Sync needed',
      short: 'Remote changed · sync needed',
      detail: 'The local branch is behind or diverged from origin. Fetch and integrate the remote changes before pushing.',
      tone: 'changed', actionable: true,
    },
    detached_choose_branch: {
      row: 'Choose branch',
      short: 'Choose branch before push',
      detail: 'This local commit is not known on origin and HEAD is detached. Switch to or create a branch before pushing.',
      tone: 'changed', actionable: true,
    },
    origin_missing: {
      row: 'No origin',
      short: 'No origin · configure remote',
      detail: 'This submodule has no origin remote. Configure its own repository remote before trying to publish the commit.',
      tone: 'changed', actionable: true,
    },
    on_origin_stage_project: {
      row: 'New version · stage',
      short: 'New version on origin · stage project',
      detail: 'The submodule commit is already on origin. Stage the new submodule reference in the main project.',
      tone: 'new-version', actionable: true,
    },
    on_origin_commit_project: {
      row: 'New version · commit',
      short: 'New version on origin · commit project',
      detail: 'The new submodule reference is staged in the main project. Commit the main project to record it.',
      tone: 'new-version', actionable: true,
    },
    project_commit_push_needed: {
      row: 'Parent push pending',
      short: 'Parent project commit · push pending',
      detail: 'The submodule itself is already in sync. The main project has a local commit that records this submodule reference and that parent commit still needs to be pushed.',
      tone: 'unpushed', actionable: true,
    },
    unavailable: {
      row: 'Unavailable',
      short: 'Submodule unavailable',
      detail: 'The submodule repository or its current commit could not be read.',
      tone: 'changed', actionable: true,
    },
    status_unknown: {
      row: 'Status unknown',
      short: 'Status unavailable · refresh',
      detail: 'The relationship between the local commit and origin could not be determined from the known Git references.',
      tone: 'changed', actionable: true,
    },
    synced: { row: 'Tracked', short: 'Tracked', detail: 'The submodule, origin and main project reference are synchronized.', tone: '', actionable: false },
  };

  function presentation(code) { return states[code] || states.synced; }
  return { presentation };
});
