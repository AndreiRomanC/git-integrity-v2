# Local Drive merge boundary

`local_drive.rs` owns the dual-pane file manager, text decoding/encoding and
single-item filesystem operations. `merge.rs` owns recursive two-folder
comparison and the guarded left-to-right folder merge operations.

## Safety invariants

- Folder scanning is read-only and never follows symbolic links or enters
  `.git` metadata.
- The left side is always the source and the right side is always the result.
- Identical files require no work; right-only files are kept by default.
- A new right file is created exclusively and can never overwrite an item that
  appeared after the scan.
- Replacing an existing right file requires its content fingerprint from the
  scan. A stale destination aborts without writing.
- Text saving uses the same stale-content guard and stages complete output next
  to the destination before replacement.
- UI previews and line/hunk actions change memory only. Disk writes always have
  a separate, explicit save or file-level confirmation.

There is intentionally no unattended "merge all" operation. A two-way folder
comparison has no common ancestor from which to infer author intent, so every
non-identical destination is reviewed independently.
