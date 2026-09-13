---
paths: .github/workflows/*.{yml,yaml}
description: "Pin external GitHub Actions and reusable workflows to full commit SHAs, pin container actions to digests, and update pins through reviewed automation."
---

# GitHub Actions Versioning

## Pin external actions and reusable workflows to full commit SHAs (Required)

Pin each external action and reusable workflow to a full-length commit SHA. Add its release tag as a
trailing comment so reviewers can identify the version.

```yaml
- uses: actions/checkout@<full-commit-sha> # vX.Y.Z

jobs:
  deploy:
    uses: owner/repository/.github/workflows/deploy.yml@<full-commit-sha> # vX.Y.Z
```

A tag can move after review. GitHub identifies a full-length commit SHA as the only immutable action
release and applies the same hardening guidance to third-party reusable workflows. See
[Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions)
and [Reuse workflows](https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows).

## Pin container actions to image digests (Required)

Pin each container action to an immutable image digest.

```yaml
- uses: docker://ghcr.io/owner/action@sha256:<digest>
```

GitHub's hardening guidance does not state a container-digest requirement. This project rule extends
GitHub's commit-SHA guidance to container images. Dependabot does not update `docker://` references,
so maintainers must track and review digest updates manually. See
[GitHub's Dependabot support notes](https://docs.github.com/en/actions/reference/security/secure-use#keeping-the-actions-in-your-workflows-secure-and-up-to-date).

## Scope

External references include `actions/*`, vendor actions, actions from another repository in the same
organization, reusable workflows from another repository, and `docker://` container actions.

Repository-local actions use `uses: ./path`. That syntax accepts no ref, so commit-SHA and digest
rules do not apply.

## Toolchain channel references (Default)

When a project deliberately follows a moving Rust channel and trusts `dtolnay/rust-toolchain`,
permit `dtolnay/rust-toolchain@stable` or `@nightly` as a documented exception.

The action documents this form in its [usage guidance](https://github.com/dtolnay/rust-toolchain).

## Pin toolchain actions for high-security repositories (Conditional)

For a high-security repository, follow the action's
[usage guidance](https://github.com/dtolnay/rust-toolchain) to pin a revision from its `master`
branch and select the channel through the `toolchain` input:

```yaml
- uses: dtolnay/rust-toolchain@<full-commit-sha> # master
  with:
    toolchain: stable
```

## Update pins through automated dependency tooling with review (Default)

Automated dependency updates with human review are the default for action and reusable-workflow
maintenance. Dependabot is one supported implementation. When a SHA line ends with a semver comment,
Dependabot updates both the SHA and that comment:

```yaml
- uses: actions/checkout@<full-commit-sha> # vX.Y.Z
```

Keep the version at the end of the comment; additional text prevents Dependabot from recognizing it.
Review the upstream release notes and run the changed workflow before merging. See
[Dependabot's SHA comment behavior](https://github.blog/changelog/2022-10-31-dependabot-now-updates-comments-in-github-actions-workflows-referencing-action-versions/).

## Remediate deprecation warnings promptly (Default)

When CI reports an action deprecation warning, update the pin through the dependency-update process
in the next maintenance change. A warning can precede a runtime cutoff that breaks the workflow.

## Why

An external action runs with the job's filesystem, token permissions, and available secrets.
Immutable pins keep reviewed code fixed, while reviewed update automation supplies security fixes
and compatibility updates without relying on edit-time version checks.
