---
paths: .github/workflows/*.{yml,yaml}
description: "Design workflow dependencies, matrices, packaging triggers, and caches around independent responsibilities while preserving required checks."
---

# GitHub Actions Workflow Design

## Separate workflows when responsibilities or triggers differ (Conditional)

When application checks, tools or diagnostic probes, and packaging have different responsibilities
or trigger policies, give each an independent workflow dependency graph (DAG). Keep related jobs in
one workflow when they share a trigger policy and form one validation process. Do not require a
separate workflow for every job.

For example, keep application lint and tests together, run scheduled compatibility probes in another
workflow, and package successful trusted commits in a packaging workflow.

## Add dependencies only for actual prerequisites (Required)

Add a `needs` edge only when a job requires another job's output or successful result. Keep
unrelated checks parallel; display order is not a dependency. Treat a required aggregate check as a
real consumer of its prerequisite results.

For example, let lint and unit tests run independently, but make a package job wait for the build
artifact it consumes.

## Use matrices for a shared step sequence (Default)

Use a matrix when entries run the same sequence with different inputs, such as operating systems or
toolchain versions. Put workload-specific steps in dedicated jobs when a matrix would require
unrelated entries to skip those steps.

For example, use a matrix for the same test recipe across supported platforms and give a dependency
audit its own job instead of adding an audit-only matrix entry.

## Give independently scheduled packaging its own trigger (Conditional)

When packaging has a different execution policy from application checks, give it its own workflow
trigger, such as a release, manual dispatch, or completion of a validation workflow. For
`workflow_run`, use `types: [completed]` and require all of the following before executing
checked-out code or consuming upstream artifacts:

- The named upstream validation workflow concluded with `success`.
- Its event matches an explicit trusted policy, such as `push` to a protected release branch.
- Its source repository matches the expected repository and its branch is allowed by that policy.

Check out `github.event.workflow_run.head_sha` as the exact tested commit; do not use the current
branch tip or the downstream run's default `github.sha`. Ensure the upstream validation actually
checked that commit. A successful pull request run alone does not establish trust.

For example, accept only successful pushes from the same repository to protected `main`, then
package that upstream run's `head_sha`. If packaging requires several validation workflows, verify
all required results for that SHA: listing multiple names under `workflow_run.workflows` triggers on
any one of them, not completion of all of them.

Limit packaging permissions and secrets to what the job requires. `workflow_run` can access secrets
and write tokens even when the upstream workflow cannot. See GitHub's
[workflow_run documentation](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run)
and [secure use reference](https://docs.github.com/en/actions/reference/security/secure-use).

## Keep cache preparation local to each workflow (Default)

Prepare dependencies and caches within each workflow. Share compatible cache keys only as an
optimization; every workflow must install or rebuild what it needs on a cache miss. Do not require
another workflow to run first merely to populate a cache. Use artifacts for required build outputs,
and preserve trust boundaries when restoring shared data.

For example, let application checks and packaging each restore dependencies and install missing
ones, even when both use a cache keyed by the same lockfile and toolchain.

## Inspect invoked recipes for hidden coupling (Required)

Before treating jobs as independent, read their invoked recipes, scripts, and nested commands.
Confirm that their dependency discovery and workspace selection match the intended job scope.

For example, a recursive dependency scanner may still include a workspace excluded by the outer
build command. Scope the scanner explicitly or put the combined operation in the job that needs it.

## Preserve required checks and validate workflow selection (Required)

Before splitting workflows or changing job names and triggers:

- Inventory existing checks and inspect branch protection and repository or organization rulesets.
- Map each required check to its replacement, including expanded matrix job names.
- Coordinate required-check changes before removing or renaming an enforced check; if settings are
  inaccessible, report the verification gap instead of assuming coverage.

Validate YAML and expressions with an Actions-aware validator, then check expected workflow and job
selection for relevant events and changed paths. Include pull requests, trusted pushes, packaging
events, and merge queues when enabled. Check failure and skip cases as well as successful runs. For
example, confirm that a tools-only change still produces every required application check or an
explicit aggregate result that enforces the intended policy.

Avoid workflow-level path filters that leave required checks pending. For a required aggregate job,
use `always()` with `needs` and explicitly inspect prerequisite results so failed or unexpectedly
skipped checks cannot produce a successful aggregate. See GitHub's
[required-check troubleshooting](https://docs.github.com/en/pull-requests/how-tos/merge-and-close-pull-requests/troubleshooting-required-status-checks).

## Update artifact download instructions when packaging moves (Conditional)

When workflow changes move or rename artifacts, update download instructions and automated consumers
for the producing workflow, artifact names, and run selection. Select artifacts by the intended
commit and producing run; do not assume the latest run packaged the desired revision.

For example, tell users to select the successful packaging run for their commit and download its
named archive, rather than looking in the application-check run.
