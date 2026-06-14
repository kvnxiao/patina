# PR body reference: `/speccy-ship` markdown template

`REPORT.md` wraps a `<report>` root with one `<coverage>` child per
surviving requirement so `speccy verify`'s `RPT-*` lint family can
proof-shape it. GitHub's markdown renderer does not strip those
custom elements, so when the raw `REPORT.md` is piped through
`gh pr create --body "$(cat ...)"` the angle-bracket wrappers leak
into the rendered PR page as visible prose. Reviewers either decode
the XML by eye or click through to `REPORT.md` in Files-changed.

This file is the canonical markdown PR body template. The
`/speccy-ship` recipe fills the placeholders from on-disk artifacts
(`SPEC.md`, `REPORT.md`, spec-dir path) and passes the rendered
markdown to `gh pr create --body-file` instead.

## Scope: one SPEC per PR

The template assumes a single SPEC per pull request: one
`spec-dir`, one summary, one coverage table. Branches that bundle
multiple SPECs, or carry unrelated precursor commits, fall back to
a hand-authored PR body. The template can still serve as a per-SPEC
starting skeleton when hand-authoring — copy the rendered output
for each SPEC and stitch the sections — but the recipe does not
prescribe multi-SPEC composition. The agent reads the branch shape
at decision time and picks the path that matches.

## Title

The PR title is the SPEC id and slug, joined by a space, sourced
from `SPEC.md`'s frontmatter `id` and `slug` fields:

```
<SPEC-NNNN> <slug>
```

Passed to `gh pr create --title "<SPEC-NNNN> <slug>"`. The title
shape is unchanged from the pre-template recipe.

## Body template

The template has three placeholders, written as angle-bracket
tokens (`<spec-dir>`, `<summary>`, `<coverage-rows>`) so they do
not collide with MiniJinja's double-brace expression syntax. Fill
each one per `## Filling the placeholders` below and write the
result to a scratch file passed to `gh pr create --body-file`:

```markdown
## Summary

<summary>

## Coverage

| Req | Result | Scenarios | Retries |
|---|---|---|---|
<coverage-rows>

## Test plan

- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [x] `cargo +nightly fmt --all --check`
- [x] `cargo deny check`
- [x] `speccy verify`

## Reference docs

- [SPEC.md](<spec-dir>/SPEC.md)
- [REPORT.md](<spec-dir>/REPORT.md)
- [journal/](<repo-url>/tree/<head-branch>/<spec-dir>/journal)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

The body has exactly four top-level `##` headings in this order:
`## Summary`, `## Coverage`, `## Test plan`, `## Reference docs`.
Do not reorder, rename, or add sections.

## Filling the placeholders

### `<spec-dir>`

The spec-local directory, relative to repo root —
`.speccy/specs/NNNN-slug`. Sourced from the `spec_md_path` field of
`speccy next SPEC-NNNN --json` (strip the trailing `/SPEC.md`).
Used in the three Reference docs bullets to produce repo-relative
links GitHub renders as clickable file paths.

**The Reference docs entries must be markdown links, not inline
code.** Copy the template's link shapes verbatim and substitute the
placeholders — do not render the paths as `` `.speccy/.../SPEC.md` ``
(inline code). Inline code is not clickable on GitHub; the whole
point of the section is to give reviewers one-click access to the
spec artifacts.

**File links use relative paths.** GitHub resolves relative paths in
PR bodies against the repo root on the PR's head branch, so the
`SPEC.md` and `REPORT.md` bullets do not need a
`https://github.com/...` prefix.

**Folder links require absolute URLs.** A relative folder path like
`(<spec-dir>/journal/)` does **not** work — GitHub interprets it as
a branch comparison and redirects to a useless
`/compare/...journal?expand=1` page. The folder bullet must use the
absolute `tree/<head-branch>/...` form so the link lands on the
folder browser. This is why the template's `journal/` bullet uses
the `<repo-url>` and `<head-branch>` placeholders.

After substitution, the three bullets look like the following
(illustrative example — `SPEC-0042`, the slug `widget-render-timeout`,
the repo `acme/widget`, and the branch `feature/example-branch` are
fictional; substitute your own spec ID, slug, repo URL, and head
branch at fill time):

- `[SPEC.md](.speccy/specs/0042-widget-render-timeout/SPEC.md)`
- `[REPORT.md](.speccy/specs/0042-widget-render-timeout/REPORT.md)`
- `[journal/](https://github.com/acme/widget/tree/feature/example-branch/.speccy/specs/0042-widget-render-timeout/journal)`

### `<repo-url>`

The GitHub repository URL without a trailing slash —
`https://github.com/<owner>/<repo>`. Sourced from
`gh repo view --json url --jq .url` at ship time. Used only in the
absolute-URL form of the Reference docs `journal/` bullet (folder
links require absolute URLs; see `<spec-dir>` above).

### `<head-branch>`

The current feature branch name — the branch that will become the
PR's head ref. Sourced from `git branch --show-current` at ship
time. Used only in the absolute-URL form of the Reference docs
`journal/` bullet.

### `<summary>`

The prose body of the `## Summary` section in `SPEC.md`, copied up
to (but not including) the next `##` heading. Do not paraphrase,
condense, or re-author — the SPEC's summary is the source of truth
for what shipped. If the SPEC summary is multiple paragraphs, copy
all of them, preserving paragraph breaks.

**Unwrap intra-paragraph line breaks.** SPEC.md prose is authored
at ~70 columns for readability in the editor, but GitHub renders
PR bodies as GitHub-Flavored Markdown where a single newline
inside a paragraph becomes a `<br>` — so a verbatim paste comes
through wrapped at the SPEC's authored column rather than the
viewer's viewport. When filling the placeholder, within each
paragraph collapse runs of whitespace (including single
newlines) to a single space, and preserve blank lines as
paragraph separators. Preserve fenced code blocks, lists, block
quotes, and any other structural markdown inside the summary
verbatim — only soft line breaks inside prose paragraphs unwrap.

Worked example. The SPEC.md Summary fragment

```
Speccy's `resources/` template tree carries canonical sources for the
`reconcile-policy`, `retry-shape`, and `vet-phases` rules under
`modules/references/` and `modules/skills/partials/`.
```

becomes the single PR-body paragraph

```
Speccy's `resources/` template tree carries canonical sources for the `reconcile-policy`, `retry-shape`, and `vet-phases` rules under `modules/references/` and `modules/skills/partials/`.
```

so GitHub's renderer wraps it to the viewer's viewport.

### `<coverage-rows>`

One markdown table row per `<coverage>` element in `REPORT.md`, in
the order the elements appear (the canonical REPORT.md shape lists
them in numerical requirement order). Each row has four columns:

- **Req**: the `req` attribute value (e.g. `REQ-NNN`).
- **Result**: the `result` attribute value (one of `satisfied`,
  `partial`, `deferred`).
- **Scenarios**: the `scenarios` attribute value split on
  whitespace and re-joined with `, ` (e.g. `CHK-NNN, CHK-NNN`).
  An empty `scenarios` attribute renders as an empty cell.
- **Retries**: the value of the `Retry count:` line that closes
  the element body, copied verbatim including any per-task
  parenthetical breakdown (e.g. `0`, `2 (T-NNN: 1, T-NNN: 1)`).

Example rendered rows, continuing the `SPEC-0042` worked instance
(REQ-001 took one retry round for the round-1 off-by-one; REQ-002 and
REQ-003 landed clean):

```markdown
| REQ-001 | satisfied | CHK-001, CHK-002 | 1 |
| REQ-002 | satisfied | CHK-003, CHK-004 | 0 |
| REQ-003 | satisfied | CHK-005, CHK-006 | 0 |
```

## Anti-patterns

- **No raw-XML paste.** Do not use `gh pr create --body "$(cat ...
  REPORT.md)"`. GitHub does not render `<report>` / `<coverage>` as
  HTML; the angle brackets leak into the PR page.
- **No fabricated rows.** Every Coverage row corresponds to a
  `<coverage>` element actually present in `REPORT.md`. Do not
  invent rows for requirements REPORT.md omits, and do not drop
  rows for requirements REPORT.md includes.
- **No inline-code Reference docs paths.** The three Reference
  docs bullets must be markdown links (`[SPEC.md](<spec-dir>/SPEC.md)`),
  not inline code (`` `.speccy/specs/.../SPEC.md` ``). Inline code is
  not clickable on GitHub — rendering the paths that way defeats the
  section's purpose. Copy the template's link syntax verbatim and
  only substitute the `<spec-dir>` placeholder.
- **No edits to the Test plan checklist.** The five items are
  fixed: the four `AGENTS.md` "## Standard hygiene" gates plus
  `speccy verify`. They are pre-checked because the ship recipe
  halts before `gh pr create` if any gate failed — a PR reaching
  this step necessarily cleared all five.
- **No dropping the footer.** The `🤖 Generated with [Claude Code]`
  attribution line closes the body. Codex hosts swap the link
  target for the Codex CLI equivalent; no host omits the footer.
