# Workspaces

A workspace is a folder a session works in. The runtime tracks it, fences it, and can merge it into another or move it with a session.

| `kind` | Is | Removing it |
| --- | --- | --- |
| `dir` | A folder that existed before Parrot | Forgets it; never deletes it |
| `worktree` | A git worktree the runtime created from a repository | Removes the worktree, with the same checks as git |
| `external` | A workspace another system manages, such as a T3 project | Forgets it; no git operations |

## Creating one

`session/new` takes a `workspace` request, and `workspace/create` makes one without a session:

```json
{ "kind": "worktree", "repo": "/home/me/src/notes", "branch": "parrot/first-session", "base": "main" }
```

| Request | Makes |
| --- | --- |
| `{ "kind": "dir", "path": "…" }` | A workspace on an existing folder |
| `{ "kind": "worktree", "repo": "…", "branch": "…", "base": "…" }` | A git worktree; the branch defaults to `parrot/<session id>`, or `parrot/<workspace id>` when no session creates it |
| `{ "kind": "use", "workspace": "wsp_…" }` | A session joins a workspace that exists |

The `parrot new` command sends a `dir` request for its `--dir` folder, which defaults to the current folder.

## Fences

A workspace must be inside the creating principal's ceiling `paths`. The runtime checks that when it creates a `dir` or `worktree` workspace, and refuses one outside with `forbidden`.

The protocol also says a workspace must be inside one of its environment's roots, which default to the owner's home folder. The runtime reports no `roots` for its environment and does not check a roots list today, so the ceiling is the fence it enforces. [Workspaces in the protocol](../spec/protocol.md#8-workspaces) states the full rule.

This stops a token or a child session from creating a workspace outside what its grant allows.

## Status and merge

`workspace/status` reports the branch, the commit, how far ahead or behind it is, and the changed paths. `workspace/merge` merges one workspace into another in the same repository and environment, by `merge` or `squash`. A conflict leaves the target unchanged and lists the files.

## Lifetime

Worktrees live until they are removed. Stopping or deleting a session never removes its workspace, so work survives a stopped CLI. `workspace/remove` refuses a workspace that sessions use, that has uncommitted changes, or that has commits no other branch reaches. `force` needs the `destructive` scope. A `dir` or `external` workspace is only forgotten.

## Checkpoints

For a `worktree` workspace, the runtime records the workspace at the start of each turn as a commit on a hidden ref, `refs/parrot/<session>/<turn>`. The checkpoint includes tracked changes and untracked files that git does not ignore, and leaves the index and working tree alone. `session/rewind` with `files: true` restores the workspace from the checkpoint at that turn. Checkpoints are pruned with the session or after 30 days, and they are never transferred.

## Moving a workspace

`workspace/transfer` sends a workspace to another runtime over a link. Commits travel as a git bundle, and staged and unstaged changes as a patch. Untracked files travel only with `include: ["untracked"]`. Nothing is pushed to a git remote, and a non-git workspace can't be transferred.

The `to` field names the peer's environment, which the link must offer. The transfer needs the `workspace` scope, and a runtime that received the call over a link can't pass the workspace on to a third ([Links](links.md)).
