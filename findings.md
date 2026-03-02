# Findings

## Analysis of Existing Code
- `src/bin/server.rs`: The gRPC server is the main entry point. `EdgeGatewayConfig` is already imported and parsed from CLI args, but `EdgeGateway` itself is not initialized or run.
- `src/hub/edge_gateway.rs`:
    - `EdgeGateway` struct exists with `new` and `run` methods.
    - `run` contains an infinite loop `listener.accept()`.
    - `handle_connection` manually reads 21 bytes for header, then reads payload. This is fragile and hard to test.
    - `handle_session_request` currently handles `UploadData` by manually constructing SQL strings, which is vulnerable to injection and inflexible.
    - The `run` method spawns a background task for session cleanup, but it's inside the run loop (should be outside or managed better). Wait, actually looking at the code, it spawns the cleanup task *before* the accept loop, which is correct. However, the `run` method takes ownership of `self` (`self: Arc<Self>`), which is slightly unusual for a run method but works for spawning.

## Design Decisions
- **Codec**: Use `tokio_util::codec` to implement `EdgeFrameCodec`. This separates framing logic from business logic.
- **Dispatch**: The `handle_connection` should use `Framed` stream to read `EdgeFrame`s.
- **Integration**: In `server.rs`, `EdgeGateway::run` should be spawned as a separate tokio task, so it doesn't block the gRPC server.
