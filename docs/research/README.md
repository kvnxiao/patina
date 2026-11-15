# Patina contract research

These documents support the [specification](../../SPEC.md). Each records its inspected revision, evidence, intended-contract decisions, and limits. Observed implementation defects do not change the intended requirements.

| Topic | Questions answered |
| --- | --- |
| [Configuration and targets](configuration-and-targets.md) | How do manifests, profiles, variables, expressions, paths, trees, and ignores select desired targets? |
| [Transactions and recovery](transactions-and-recovery.md) | What survives interruption, rollback, ownership changes, cleanup failure, and retention? |
| [Remote sources](remote-sources.md) | How do pins, content trust, update gates, caches, and stale observations interact? |
| [Commands and platforms](commands-and-platform.md) | Which command combinations, repository writers, and Windows integrations need distinct output or consent rules? |
