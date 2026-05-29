# GitHub Actions Versioning

When authoring or editing files under `.github/workflows/`, **always
pin third-party actions to the latest published major version**.
A `@v4` reference that still works is technically fine, but if the
maintainers have shipped `@v5`, we bump to `@v5` rather than letting
the older version rot.

## Why

GitHub deprecates Node.js runtime versions periodically (Node 16 →
Node 20 → Node 24, etc.). Older action majors usually run on older
Node majors and surface as `Node.js N actions are deprecated`
warnings on every CI run. Letting them accumulate means:

- Annotations clutter the run UI and bury real warnings.
- A future GitHub-side hard cutoff turns a workflow that "works fine"
  into a hard failure with no warning window.
- The dependency surface drifts; security fixes published only on the
  newer major never reach us.

Bumping is cheap; staying current is free with one PR per quarter at
most.

## How

Two acceptable pinning forms:

```yaml
# Floating major tag — preferred for low-blast-radius CI work.
- uses: actions/checkout@v5
- uses: Swatinem/rust-cache@v2
```

```yaml
# Commit SHA — preferred when the workflow handles secrets, deploys,
# or otherwise has a wider blast radius. Comment the version it
# corresponds to so future bumps stay legible.
- uses: actions/checkout@692973e3d937129bcbf40652eb9f2f61becf3332 # v5.0.0
```

**Avoid** floating un-versioned references (`@main`, `@master`); they
silently move and turn workflows non-reproducible.

When upgrading:

1. Check the action's repo for the current latest major (`gh release
   list --repo <owner>/<action>` or the README badge).
2. Bump the tag (or update the SHA + version comment).
3. Read the upstream changelog for the major bump — breaking changes
   between majors are real and skip-reading them is how surprises
   land.
4. Run the workflow on the bump branch before merging.

## Scope of "third-party"

This rule applies to any `uses:` reference. It includes:

- `actions/*` (GitHub-maintained but still external).
- Vendor actions (`Swatinem/rust-cache`, `EmbarkStudios/cargo-deny-action`,
  `dtolnay/rust-toolchain`, etc.).
- Reusable workflows from other repos.

It does **not** apply to `dtolnay/rust-toolchain@stable` /
`@nightly` style channel references — those are deliberately
floating-by-channel rather than version-pinned, and the upstream
contract is that they track the named Rust channel.

## Reciprocal expectation

When you encounter a deprecated-action warning in CI logs, fix it
that turn rather than queuing it. The whole point of this rule is to
keep the deprecation backlog at zero.
