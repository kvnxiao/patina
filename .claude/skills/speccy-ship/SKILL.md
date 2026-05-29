---
name: speccy-ship
description: 'Close out a Speccy spec — write REPORT.md, run `speccy verify` as a CI dry-run, commit, and open a pull request. Use when the user says "ship SPEC-NNNN", "wrap up the spec", "open a PR for the spec", or every task is done and the loop is complete. Requires: all tasks `state="completed"`. If any task is still pending → prefer speccy-work to finish them first. Do NOT trigger while any task is still `pending` or `in-progress`.'
---

# /speccy-ship

Read `.claude/agents/speccy-ship.md` and follow it, or invoke
`/agent speccy-ship` for the pinned execution path.
