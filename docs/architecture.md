# Architecture

This project follows a Clean-Architecture-flavored layering adapted for a
single-binary desktop application.

```
┌──────────────────────────────────────────────────────────────────┐
│  app  (binary, composition root)                                 │
│   ├─ bridge/    Slint ↔ worker glue                              │
│   └─ view_model/ Presentation-shaped data                        │
└────────────┬──────────────────────────────┬──────────────────────┘
             │                              │
             ▼                              ▼
       ┌────────────┐                ┌──────────────┐
       │    ui      │                │   worker     │
       │  (Slint)   │                │   (tokio)    │
       └────────────┘                └──────┬───────┘
                                            │
                                            ▼
                                     ┌──────────────┐
                                     │   engine     │
                                     │ (MergeEngine │
                                     │   trait)     │
                                     └──────┬───────┘
                                            │
                                            ▼
                                     ┌──────────────┐
                                     │     core     │
                                     │  (domain)    │
                                     └──────────────┘
```

## Layer rules

| Crate           | May depend on              | Must NOT depend on             |
|-----------------|----------------------------|--------------------------------|
| `core`          | (nothing app-specific)     | `engine`, `worker`, `ui`, OS   |
| `engine`        | `core`                     | `worker`, `ui`                 |
| `worker`        | `core`, `engine`           | `ui`                           |
| `persistence`   | (nothing app-specific)     | `worker`, `engine`, `ui`       |
| `ui`            | (only generated Slint)     | `worker`, `engine`, `core`     |
| `app`           | everything                 | —                              |

Dependencies always point inward. Domain types never know about FFmpeg,
Tokio, or Slint.

## Swap-point

The single seam between domain and infrastructure is the `MergeEngine`
trait in `engine/src/traits.rs`. Replace `FfmpegEngine` with a different
implementation in `app/src/main.rs` and the rest of the workspace is
unchanged.
