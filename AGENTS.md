# Download provider contract

The shared scheduler, task state, and provider traits are in `src/backend/src/worker.rs` and `src/backend/src/worker/queue.rs`. BaiduPCS-Go belongs in `src/backend/src/worker/baidu/`. Add a new provider in its own module under `worker/`. Register it in `src/backend/src/main.rs`.

## Implement the adapter

- Implement `DownloadProvider`, `DownloadSession`, and `DownloadTransfer`. Give the provider a stable, unique ID.
- Return a stable account ID from the connected session. Keep credentials in provider configuration and keep them out of API responses and logs.
- Implement `current` to compare the account credentials, monitored scope, and local destination with the latest configuration. Use `contains` to reject tasks outside the monitored scope. Check the scope again just before a cloud mutation.
- Return a complete list of files from `list`. Read all pages and subdirectories in the configured scope. Each record contains an opaque remote file ID, a canonical remote path, and a size in bytes.
- Publish the whole list before starting a transfer. Include zero-byte files. Directories do not become tasks. If the upstream list rounds sizes, document its precision and replace the estimate when exact metadata is available.
- Return an error for a failed or incomplete scan. This includes timeouts, authorization failures, malformed output, missing pages, and duplicate paths. Return an empty list only when the provider confirms that the scope is empty or absent.
- Start exactly one file per transfer. Verify the remote identity before starting. Preserve the configured local destination and nested paths. Reject paths outside the download root, traversal, and ambiguous path syntax. Pass exact paths to tools that support patterns.
- Report byte counts and bytes per second through `TransferProgress`. Report `Active` or `Retrying` during a transfer and `Complete`, `Existing`, or `Failed` at the end. The shared queue owns `Queued` and `Paused`.
- Return `Complete` after the transfer exits and the adapter confirms success. Some tools print a completion message before integrity checks or return exit code zero after an error. Account for both cases.
- Make `next` safe to interrupt between polls. Preserve any consumed input needed by the next poll. Make `stop` wait for the transfer to stop. Dropping a transfer must also stop any owned process. Preserve local resume data so a restarted transfer can reuse it.

## Queue behavior

The scheduler uses provider, account, remote ID, and path to identify a task. A replacement file with a new ID receives a new task. A rename creates a task at the new path. The scheduler sorts a new scan by size and then path, and runs one transfer at a time per provider.

- Pause a queued task without stopping another transfer. Pause an active task by saving its disposition in SQLite before stopping its transfer. Keep its partial data and resume information.
- Resume a paused task by clearing its saved disposition and putting it back in the queue. Reuse local progress when the provider supports resume.
- Remove a task by saving a removal disposition. Exclude that identity from later scans and after a service restart. Keep its local files, including partial data.
- Reconcile the queue after each complete scan. Archive a known task if its cloud file disappeared. Record the remote deletion reason and retain any saved byte count. If the provider lists the same file ID at another path, record the move. Stop the transfer if it was active. Continue scanning during long transfers.
- Keep the last byte count while the task waits for a retry. Keep the last failure reason until another attempt succeeds or reports a different failure. Use a structured failure reason. Keep raw provider output out of the API.
- Show the next scan time for a waiting task. A retry requires a successful full scan and an available transfer slot. Keep the current round free to finish its other tasks.
- Keep the prior queue on a failed scan. Do not interpret a connection problem as a cloud deletion. On an account or scope change, prevent stale tasks from mutating the new cloud scope.

Record observed tasks in SQLite before starting downloads. Move each finished task to history after its outcome is saved. Keep archived identities out of the active queue. Preserve the completion reason after a successful download, even if the cloud original is recycled later.

## Cloud recycling

Expose separate, provider-specific settings for recycling after completion and recycling on manual removal. Both settings default to off. Explain that manual removal can recycle an incomplete download.

- For manual removal, check that the request matches the saved setting. Save the paused disposition and stop the selected transfer before asking the provider to recycle its original.
- Use the provider recycle bin. Verify the exact file identity immediately before the request. Never expand a pattern or remove a directory in place of a file.
- Treat confirmed absence as a successful manual removal. If recycling fails or the result is uncertain, keep the task paused so the user can retry. If a later full scan confirms absence, remove the row.
- For automatic recycling, wait for a successful download. Verify the local regular file, expected path, exact size, and provider checksum. Refuse to recycle a file when metadata is missing, the local copy changed, a resume marker remains, or an existing file was skipped.
- Keep local verification in `after_complete`. Keep manual recycling in `recycle`. When the provider offers only path-based recycling, use a fresh identity lookup and document the remaining race between lookup and mutation.

BaiduPCS-Go v4.0.2 provides rounded sizes in its detailed directory listing and exact bytes in file metadata. It writes a `.BaiduPCS-Go-downloading` marker for partial downloads. Its recycle operation accepts paths. A fresh metadata response with code `31066` confirms absence. Keep these protocol details in the Baidu adapter.

## Web and tests

Use the authenticated `POST /api/v1/downloads/status` and `POST /api/v1/downloads/update-download` endpoints for all providers. The status response contains provider phases, removal policy, and active task rows. Set `include_completed` to include archived rows for each connected account. Completed rows are read-only. Keep the default view on active tasks. Each action sends the provider ID, opaque task ID, and action. A removal also sends the policy the user saw. Reject a stale policy and an unknown task.

Keep provider configuration endpoints separate. Keep the shared table, column resizing, action buttons, and translations reusable. Place the action column directly after status. Show a size and 0% progress for queued rows with no bytes saved. Read local checkpoints through the adapter when available. Label local file sizes that lack verification. Show pause or resume where applicable and use a cross for removal. Show cloud recycling errors without hiding the paused task.

Put tests in the mirrored `tests/backend/src/` tree. Cover listing formats, nested paths, sizes before transfer, account isolation, and stale identities. Test queued and active pause, resume, restart persistence, both removal policies, cloud disappearance during a transfer, and a failed scan that preserves the queue. Test incomplete local files, checksum mismatches, and ambiguous recycle results.
