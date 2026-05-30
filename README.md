# 4K Video Merger

A high-performance desktop application for merging 4K video files with lossless
stream-copy where possible, and intelligent re-encoding fallback when codecs
mismatch. Built in Rust with a Slint UI.

## Features & Roadmap

- [x] Project Scaffolding & Architecture Design
- [x] UI Shell (Slint Layout, Sidebar, and Styling)
- [x] File Dropper / Media Import Component
- [x] Timeline Playlist & Reordering System
- [x] Rust Background Worker Thread (Task Queue)
- [x] FFmpeg Stream Copy Engine (Lossless Merge Logic)
- [x] FFmpeg Re-encoding Engine (Fallback for mismatched codecs)
- [x] Export Progress Bar & Status Reporting
- [x] Local App Configuration & History Persistence

## Architecture

This project is organized as a Cargo workspace with the following crates:

| Crate          | Layer            | Responsibility                                  |
|----------------|------------------|-------------------------------------------------|
| `app`          | Composition root | Binary entry; wires UI, worker, and engine      |
| `ui`           | Presentation     | Slint markup (`.slint` files)                   |
| `core`         | Domain           | Pure business rules: playlists, compatibility   |
| `engine`       | Infrastructure   | FFmpeg / GStreamer process wrappers             |
| `worker`       | Concurrency      | Tokio runtime and job dispatcher                |
| `persistence`  | Infrastructure   | Local config and export history                 |

See [`docs/architecture.md`](docs/architecture.md) and
[`docs/threading-model.md`](docs/threading-model.md) for details.

## Prerequisites

- Rust 1.88+ (pinned to `stable` via `rust-toolchain.toml`)
- FFmpeg 6.0+ available on `PATH`

## Build & Run

```bash
cargo run -p video-merger-app --release
```

## Project Structure

```
crates/
├── app/          # Binary: bridges UI and backend
├── ui/           # Slint UI components
├── core/         # Domain logic (no I/O)
├── engine/       # FFmpeg backend
├── worker/       # Background task queue
└── persistence/  # Local storage
```

## License

MIT OR Apache-2.0
# cutoff
# cutoff
