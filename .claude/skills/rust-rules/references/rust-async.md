---
paths: **/*.{rs,toml}
description: "Async Rust contracts for APIs, task ownership, cancellation, resource limits, shutdown, retries, diagnostics, and deterministic tests; conditional Tokio guidance."
---

# Async Rust

Apply the runtime-independent contracts to async code under any executor. Apply the Tokio guidance
only when the project uses Tokio. Keep compilation, tests, and Clippy on the
[stable baseline](rust-lints-and-formatting.md#share-local-fix-lint-and-ci-tasks-required), with
nightly rustfmt.

## Runtime-independent contracts

### Introduce async for required behavior (Default)

Keep bounded synchronous computation synchronous. Introduce async for asynchronous waiting or an
existing async interface. Prefer `std` and existing dependencies when they provide the required
behavior; follow the [dependency policy](rust-dependencies.md#evaluate-before-adding-default).

When an executor is needed, reuse the project's runtime. Default new general-purpose async I/O
applications to Tokio when its capabilities fit; choose another executor for platform or execution
requirements it does not support. A library can expose futures without depending on a runtime, but
`std` does not provide a general-purpose async executor. See the
[Rust Book on runtimes](https://doc.rust-lang.org/book/ch17-01-futures-and-syntax.html#executing-an-async-function-with-a-runtime).

Add runtime adapters, synchronous counterparts, or executor traits only for supported caller needs.

### Establish public future contracts (Required)

State runtime requirements and choose the future's intended auto-trait contract for supported
callers. Promise `Send` when the calling model requires moving futures between threads; permit
non-`Send` futures for local execution. Keep borrowing APIs where useful, and require `'static` only
when the operation's ownership requires it. Verify promised future bounds with
[auto-trait tests](rust-testing.md#auto-trait-and-drop-count-tests-default) and representative
generic callers.

For a public trait requiring `Send` futures, express the bound in its method signature, such as `fn
read(&self) -> impl Future<Output = io::Result<Vec<u8>>> + Send`. Native `async fn` trait methods do
not promise `Send` futures to generic callers. Introduce trait-object erasure or an adapter when
dynamic dispatch is required; native async and return-position `impl Trait` methods are not
dispatchable through `dyn Trait`. Use concrete future boxing when recursion or future-size
constraints require indirection; see
[recursive futures](https://doc.rust-lang.org/error_codes/E0733.html). Follow the existing lint
exception policy for intentional local-only native async traits. See
[`async_fn_in_trait`](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html#async-fn-in-trait)
and [dyn compatibility](https://doc.rust-lang.org/reference/items/traits.html#dyn-compatibility).

When async methods need dynamic dispatch, consider
[`async-trait`](https://docs.rs/async-trait/latest/async_trait/) for the boxed-future adapter. When
callers need both local and `Send` trait variants, consider
[`trait-variant`](https://docs.rs/trait-variant/latest/trait_variant/). Keep native signatures when
neither capability is needed.

### Keep executor workers responsive (Required)

Move blocking I/O and substantial uninterrupted CPU work off executor workers. Review synchronous
callees and destructors as well as the async function body. Keep small, bounded computation inline;
choose offloading from the workload and responsiveness requirements rather than a universal time
threshold. `Future::poll` must return promptly; see the
[`Future` contract](https://doc.rust-lang.org/std/future/trait.Future.html#the-poll-method).

When repeated submissions can accumulate, bound admission before spawning or submitting work. An
existing fixed worker set or upstream admission limit can establish the bound. If a permit limits
running work, retain it with that work until completion, including when its caller stops waiting.

### Own spawned work and its outcomes (Required)

Give each spawned task an owner responsible for its operation result, task failure, and termination.
When work must outlive its initiating scope, transfer responsibility to a longer-lived owner. Use
directly awaited futures when independent scheduling or a longer lifetime is unnecessary; choose
task handles, groups, or existing supervisors according to the lifecycle.

Observe both task-join failures and errors returned by the operation. Preserve their sources under
the [error propagation policy](rust-error-handling.md#add-context-eager-vs-lazy-default), and
distinguish cancellation, timeout, panic, and operation failure when callers react differently.
Report failures at the boundary responsible for them.

### Preserve a valid outcome on cancellation (Required)

At selection, timeout, abort, and parent-cancellation boundaries, establish what happens when a
pending future is dropped. Preserve partial progress, continue the operation under an accountable
owner, or safely abandon the affected state and resources. Before reusing a resource, verify that
partial reads, writes, and state changes preserve its invariants.

Document cancellation contracts that callers need to compose public operations. Test controlled
suspension points where dropping work can leave observable state. Require restart safety only when
the operation will be restarted; abandoning an entire connection can make partial protocol progress
irrelevant. Treat remote side effects as potentially committed when their outcome is unknown.

### Bound resources at admission (Required)

Where input can grow without limit, bound active work, queued work, pending producers, and retained
payloads. Validate variable payload sizes when message counts alone cannot bound memory. Reuse
inherent bounds for fixed workloads rather than adding another limiter.

For example, spawning a task per input and acquiring a permit inside each task limits active work
but leaves waiting tasks and their inputs unbounded. Acquire capacity before spawning, and account
for any producers waiting to acquire that capacity. Test saturation and the selected overload
outcome. See
[backpressure](https://tokio.rs/tokio/tutorial/channels#backpressure-and-bounded-channels).

When processing a stream of futures whose results may arrive in completion order, consider
[`StreamExt::buffer_unordered(n)`](https://docs.rs/futures-util/latest/futures_util/stream/trait.StreamExt.html#method.buffer_unordered)
with a positive concurrency limit. When independent scheduling requires a Tokio `JoinSet`, bound
admission and drain completions during processing; completed tasks retain results until joined.

### Use bounded queues (Default)

Default work queues to bounded capacity. Choose waiting, rejection, dropping, or coalescing from the
data contract, and test full and closed queues. Permit an unbounded channel only when another
concrete invariant bounds outstanding messages; a fixed sender count alone does not bound messages.

Distinguish enqueue success from processing completion. Add an acknowledgement only when the caller
requires a processing guarantee. When cancellation can drop an unsent value, retain or discard it
according to the operation's cancellation contract.

### Choose locks by critical-section behavior (Default)

Default short, low-contention synchronous critical sections to a synchronous mutex. Release
synchronous lock guards and `RefCell` borrows in a lexical scope before awaiting. Use an async mutex
when acquisition must suspend or exclusive access must span async operations; review lock ordering
and invariants after cancellation. Keep exclusive access only as long as the protected operation
needs.

Use a resource-owning task when message passing simplifies its protocol. Follow the existing
[ownership policy](rust-performance.md#make-ownership-transfers-explicit-default) for `Arc`, clones,
and transfers. An async context alone does not require shared ownership or an actor.

### Stop admission and observe shutdown (Required)

Stop accepting new work before initiating the agreed cancellation or drain behavior, then observe
owned task outcomes. Account for workers that can create more work. Treat a cancellation signal as a
request; wait for completion before reporting that workers stopped.

When shutdown must finish within a budget, define a grace period and an escalation policy consistent
with cancellation safety. Choose safe abort, durable handoff, recovery, or application termination
according to the work's contract. Report unresolved work rather than claiming a completed drain.
Test stopped admission, cooperative completion, and escalation. See
[graceful shutdown](https://tokio.rs/tokio/topics/shutdown).

### Preserve operation deadlines (Required)

When an operation has a completion budget, use one monotonic deadline across capacity waits,
attempts, and backoff. Pass the deadline or remaining budget only to components that need it; a
tighter stage limit must not extend the parent deadline. For example, 300 ms spent waiting for
capacity leaves 700 ms of a one-second operation budget.

For deliberately long-lived streams and workers, choose cancellation or idle/progress policies
instead of an arbitrary lifetime timeout. Test budget consumption across stages. Treat a timeout as
the end of a wait, not proof that execution stopped or external effects were rolled back. See
[deadline propagation](https://grpc.io/docs/guides/deadlines/#deadline-propagation).

### Retry only repeatable operations (Conditional)

When retries are justified, require both a retryable failure classification and safe repetition
semantics. An uncertain timeout does not prove a write was unapplied. Use protocol-supported
idempotency or evidence that the operation was not applied before repeating non-idempotent work; see
[HTTP retry semantics](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.2.2).

Bound attempts and keep capacity waits, attempts, and backoff within any operation deadline. Use
capped backoff and jitter when concurrent clients could synchronize. Account for retries already
performed by dependencies before adding an outer loop, and reuse existing client facilities when
they implement the contract. Preserve error sources when reporting exhaustion. Test terminal
failures, attempt limits, cancellation, and deadline consumption with controlled time and inputs.

### Correlate concurrent diagnostics (Default)

Instrument meaningful operation and task boundaries using the project's structured diagnostics. When
concurrent work needs correlation and no suitable facility exists, prefer `tracing`. Propagate
relevant operation context into spawned work, and choose fields that identify the operation and
outcome. Exclude secrets and unnecessary payloads. Test diagnostic content only when it is part of
the supported contract.

### Preserve tracing context and field privacy (Required)

When using `tracing`, instrument futures with `Instrument` or `#[instrument]`; never hold
`Span::enter` or `entered` guards across `await`. Enter a span with `in_scope` only for synchronous
work. `#[instrument]` records arguments by default, so skip sensitive or unnecessary arguments and
record useful fields explicitly. Leave subscriber installation to executables; use scoped
subscribers for isolated tests. See
[async spans](https://docs.rs/tracing/latest/tracing/struct.Span.html#in-asynchronous-code),
[argument capture](https://docs.rs/tracing/latest/tracing/attr.instrument.html), and
[library instrumentation](https://docs.rs/tracing/latest/tracing/#in-libraries).

For projects using `tracing`, install the
[guard lint configuration](rust-lints-and-formatting.md#check-tracing-guards-across-await-required).

### Verify async contracts deterministically (Required)

Use explicit readiness/completion signals or controlled polling for ordering assertions. Do not
infer another task's progress from elapsed sleeps or a fixed number of yields. Verify affected
cancellation, resource, shutdown, and retry contracts with controlled dependencies; keep expected
outcomes independent of the implementation. Test real I/O or multithread behavior when those
properties are part of the contract.

Retain the existing future and guard checks in the
[lint baseline](rust-lints-and-formatting.md#install-the-complete-lint-baseline-required). Review
lifecycle, admission, cancellation, and external side effects separately; passing Clippy does not
establish those contracts.

### Add concurrency tools for specific invariants (Conditional)

When custom synchronization or comparable interleaving-sensitive invariants justify model checking,
use Loom with its replacement primitives and a bounded model. Account for uninstrumented dependency
operations and memory-model limitations when interpreting results; Loom does not automatically
verify an arbitrary async application. Keep the check separate from ordinary stable tests. See
[Loom limitations](https://docs.rs/loom/latest/loom/#limitations-and-caveats).

When a test can block its executor or hang during shutdown, bound the test process with existing
runner or CI facilities. An in-runtime timeout cannot preempt non-yielding work. Follow the
[conditional nextest guidance](rust-testing.md#nextest-serialize-and-bound-flaky-tests-conditional)
when that runner fits the project.

## Tokio-specific guidance

### Keep synchronous runtime entry at explicit boundaries (Required)

Within Tokio's async execution context, await futures instead of directly calling
`Runtime::block_on` or `Handle::block_on`; those calls panic in that context. For a synchronous
adapter, reuse an owned runtime or handle rather than constructing a runtime per operation. Choose
runtime ownership for the adapter, application, or isolation requirement rather than a fixed count
per process.

When a synchronous API requires re-entry from a multithreaded Tokio runtime, use a documented bridge
such as `block_in_place` with `Handle::block_on`. Account for `block_in_place` suspending concurrent
branches within the same task, and its incompatibility with the current-thread runtime. See
[runtime handles](https://docs.rs/tokio/latest/tokio/runtime/struct.Handle.html#method.block_on) and
[`block_in_place`](https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html).

For delays on executor workers, await `tokio::time::sleep` instead of calling `std::thread::sleep`.
Keep blocking sleeps on dedicated or blocking-pool threads when the blocking operation requires
them.

### Offload finite blocking work (Conditional)

When finite blocking work needs offloading under Tokio, use `spawn_blocking` with the admission
bound established before submission. Move any execution permit into the closure and retain it until
the work finishes. The blocking-pool thread maximum does not bound queued submissions, and
cancelling the async waiter must not release capacity while the closure still runs.

In an owner's admission loop, retain the permit inside each submitted computation:

```rust
let permit = limit.clone().acquire_owned().await?;
workers.spawn_blocking(move || {
    let _permit = permit;
    compute(input)
});
```

Keep `workers` in the task owner and drain its completions while admitting work. The permit bounds
submitted computations; separately bound waiting producers and results retained in the `JoinSet`.

Use dedicated threads for persistent blocking loops. Add a specialized CPU executor such as Rayon
only when its capabilities benefit the workload. Started `spawn_blocking` jobs cannot be aborted;
runtime shutdown timeouts stop waiting without stopping those jobs. Account for their termination in
the owner's shutdown policy. See
[`spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) and
[blocking-pool limits](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html#method.max_blocking_threads).

### Observe task completion (Required)

Retain a `JoinHandle` or transfer the task to an owner that observes its outcome; dropping the
handle detaches the task. When timeout policy needs subsequent cancellation or joining, borrow the
handle into the timeout instead of consuming it. Request cancellation with `abort` only when safe,
then await the handle to observe completion. Aborting non-yielding async work does not establish a
hard termination deadline.

When using `JoinSet::abort_all`, drain the set to observe completion. Use `JoinSet::shutdown` only
when ignoring task panics matches the owner's failure policy. When using `TaskTracker`, stop
admission separately: `close()` enables waiting for an empty tracker but does not prevent new tasks.
See [task handles](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html),
[task groups](https://docs.rs/tokio/latest/tokio/task/struct.JoinSet.html), and
[task tracking](https://docs.rs/tokio-util/latest/tokio_util/task/task_tracker/struct.TaskTracker.html).

### Check cancellation at selection and timeout boundaries (Required)

Before placing an operation in `select!` or a timeout, check its documented cancellation contract.
For example, cancelling and restarting `read_exact` or `write_all` can lose tracked progress;
cancelling mutex or semaphore acquisition loses queue position. Choose progress preservation,
continued ownership, or resource abandonment accordingly. When using `biased;`, order branches to
prevent a continuously ready branch from starving cancellation or other required progress. See
[`select!` cancellation safety](https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety).

Use `timeout_at` with the operation deadline where a budget applies. A non-yielding future can
exceed its timeout and still return success; use the workload and shutdown controls above for
execution limits. See [`timeout`](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html).

### Apply channel and mutex contracts (Required)

Treat bounded `mpsc` capacity as a message-count bound. Include values retained by pending senders
and variable message sizes in the admission policy. `send` success establishes enqueueing, not
processing. When cancellation must preserve an unsent message, reserve capacity while retaining the
value outside the cancelled future. See
[`mpsc::Sender`](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html).

Return the unsent value when cancellation wins or capacity reservation fails:

```rust
async fn send_or_retain<T>(
    sender: &tokio::sync::mpsc::Sender<T>,
    message: T,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Option<T> {
    let permit = tokio::select! {
        biased;
        () = shutdown.cancelled() => return Some(message),
        result = sender.reserve() => match result {
            Ok(permit) => permit,
            Err(_) => return Some(message),
        },
    };
    permit.send(message);
    None
}
```

Await this helper to its outcome when the value must be recovered; dropping the entire helper also
drops its owned message. After reservation succeeds, send without another await point.

Apply the [lock selection rule](#choose-locks-by-critical-section-behavior-default) to Tokio code.
Tokio mutex guards can be held across `await`; verify the protected state remains valid on
cancellation. See
[mutex selection](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html#which-kind-of-mutex-should-you-use).

### Use virtual time for timer logic (Default)

Default compatible Tokio timer tests to `#[tokio::test(start_paused = true)]` with `test-util` and
`time` enabled. Paused time requires the current-thread runtime and controls Tokio's clock, not
standard-library or system clocks. Use existing timer facilities before introducing a clock trait.

Await readiness before advancing time when timer registration matters, and await the result being
asserted afterwards. `advance()` is not a task-completion barrier; paused time can also advance
automatically when the runtime has no work. Retain suitable integration tests for real I/O and
multithread behavior. See [paused time](https://docs.rs/tokio/latest/tokio/time/fn.pause.html),
[`advance`](https://docs.rs/tokio/latest/tokio/time/fn.advance.html), and
[`yield_now` non-guarantees](https://docs.rs/tokio/latest/tokio/task/fn.yield_now.html#non-guarantees).

When an assertion requires a registered, pending timer, signal readiness after its first poll:

```rust
#[tokio::test(start_paused = true)]
async fn completes_after_registered_deadline() {
    use std::future::Future;
    use std::task::Poll;
    use std::time::Duration;

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        let timer = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timer);
        std::future::poll_fn(|cx| {
            assert!(timer.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        ready_tx.send(()).expect("test awaits timer readiness");
        timer.await;
    });
    ready_rx.await.expect("worker registers its timer");
    assert!(!worker.is_finished());
    tokio::time::advance(Duration::from_secs(5)).await;
    worker.await.expect("worker completes without panicking");
}
```
