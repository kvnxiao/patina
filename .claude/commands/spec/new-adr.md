---
description: Scaffold a new ADR for a SPEC phase
argument-hint: <SPEC>/<short-title>  e.g. 0001/use-camino
---

You are creating an Architecture Decision Record (ADR).

The argument `$ARGUMENTS` is in the form `<SPEC>/<short-slug>`, for example `0001/use-camino`. Parse it.

## Step 1 — Find the next ADR number for this SPEC

1. List `specs/<NNNN>/decisions/` (excluding `README.md`).
2. Find the highest numeric prefix.
3. The new ADR is the next number, zero-padded to 4 digits.

Example: if `0001-camino-over-pathbuf.md` and `0002-fs2-for-locking.md` exist, the new file is `0003-<slug>.md`.

## Step 2 — Create the file

Path: `specs/<NNNN>/decisions/<MMMM>-<slug>.md`

Use the template at `docs/dev/adr-template.md`. Fill in what you can:

- Title (humanized from the slug).
- Status: `Proposed` (will become `Accepted` on PR merge).
- Date: today.
- SPEC / Phase: ask the user if not provided.
- Author: your agent ID + the user's handle if collaborating.

## Step 3 — Update the index

Add a row to `specs/<NNNN>/decisions/README.md`:

```
| <MMMM> | <Title> | Proposed | YYYY-MM-DD | <phase> |
```

## Step 4 — Hand off

Return the path of the new ADR file. The user will fill in the Context, Decision, Consequences, and Alternatives sections, then open a PR.

Do NOT fill in those sections speculatively — the user knows the actual reasoning behind the decision; you do not.

---

## Hard rules

- One decision per ADR. If you find yourself capturing two unrelated decisions, split them.
- Keep ADRs to one page. Long content belongs in a SPEC.
- ADRs are append-only. To revise a decision, write a new ADR that supersedes the old one and update the old one's status to `Superseded by ADR-NNNN`.
