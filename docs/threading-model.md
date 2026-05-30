
# Threading model

## Threads

| Thread                | Owner                  | Purpose                                |
|-----------------------|------------------------|----------------------------------------|
| UI / Slint event loop | `app::main`            | Render, dispatch user input            |
| Worker (tokio MT)     | `worker::WorkerHandle` | Run async jobs, talk to FFmpeg         |
| UI-event bridge       | `app::bridge::events`  | Forward worker events into Slint loop  |
| Per-job child process | tokio process pool     | Isolated FFmpeg / ffprobe processes    |

## Channels

```
UI thread ── cmd_tx (mpsc::Sender<Command>, bounded 64) ───► Worker
Worker  ── event_tx (mpsc::UnboundedSender<Event>) ───► UI-event bridge
                                                          └── slint::invoke_from_event_loop
```

- Commands flow UI → Worker. Bounded channel applies backpressure if the UI
  spams imports.
- Events flow Worker → UI. Unbounded because the producer is the engine and
  blocking it on a slow UI would be worse than buffering.
- Engine progress arrives on a third channel local to a single job and is
  forwarded into `event_tx` by `worker::progress::forward`.

## Cancellation

Each merge job receives a `tokio_util::sync::CancellationToken`. The token
is stored in the dispatcher's `cancels` map keyed by `JobId`. A
`Command::Cancel` looks the token up and cancels it; the engine checks the
token between FFmpeg I/O chunks and SIGTERMs the child if needed.

## Rules

1. **Never block the Slint event loop.** No `block_on`, no synchronous file
   I/O on UI callbacks. Anything slow goes over `cmd_tx`.
2. **Never touch Slint state from the worker.** All UI mutations must go
   through `slint::invoke_from_event_loop`.
3. **No shared mutable state across thread boundaries** except for the
   dispatcher's cancellation map (already protected by `tokio::Mutex`).
4. **`Arc<dyn MergeEngine>` is the only cross-thread handle to FFmpeg.**
   The trait is `Send + Sync + 'static` precisely so it can be cloned
   into spawned tasks.
