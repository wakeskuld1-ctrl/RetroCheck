# Edge-facing TCP Server Design (Hub Side)

## 1. Overview
This document outlines the design for the Hub-side TCP server that communicates with Edge devices.
The server uses a custom 21-byte header protocol with FlatBuffers payload.

## 2. Protocol Specification

### 2.1 Header Format (21 Bytes)
| Field          | Size (Bytes) | Type | Description                                      |
|----------------|--------------|------|--------------------------------------------------|
| Magic          | 4            | [u8;4]| Constant "EDGE" (0x45 0x44 0x47 0x45)           |
| Version        | 1            | u8   | Protocol version (currently 1)                   |
| Type           | 1            | u8   | Message Type (1=Heartbeat, 2=Data)               |
| RequestID      | 8            | u64  | Unique Request ID for correlation                |
| Payload Length | 4            | u32  | Length of the FlatBuffers payload in bytes       |
| Checksum       | 3            | [u8;3]| Reserved/Checksum (currently 0x00 0x00 0x00)    |

### 2.2 Payload Schema (FlatBuffers)
Since `flatc` is not available in the environment, we will manually implement the serialization logic in Rust, while keeping a reference `edge.fbs` for documentation.

**edge.fbs:**
```flatbuffers
namespace edge;

table Heartbeat {
  node_id: ulong;
  timestamp: ulong;
}

table DataRecord {
  key: string;
  value: [ubyte];
  timestamp: ulong;
}

table UploadData {
  records: [DataRecord];
}

union EdgeRequest { Heartbeat, UploadData }

table Request {
  req: EdgeRequest;
}

table Response {
  code: int;
  message: string;
}

root_type Request;
```

## 3. Architecture Design

### 3.1 Module Structure
- `src/hub/mod.rs`: Hub module entry.
- `src/hub/edge_gateway.rs`: Main TCP server logic.
- `src/hub/edge_schema.rs`: Manually implemented FlatBuffers structs/builders.

### 3.2 EdgeGateway Logic
- **Listener**: `tokio::net::TcpListener` on a configurable port (default 8080).
- **Connection Loop**: Spawns a Tokio task for each connection.
- **Framing**: Reads 21 bytes header, validates Magic/Version, then reads Payload.
- **Deserialization**: Uses `flatbuffers` to parse the payload into `EdgeRequest`.
- **Handling**:
  - `Heartbeat`: Logs/Updates status (No-op for now).
  - `UploadData`: Converts records to SQL (`INSERT OR REPLACE`) and writes via `RaftRouter`.
- **Response**: Sends a simple header + FlatBuffers response (OK/Error).

## 4. Implementation Steps
1.  Create `src/hub` directory and modules.
2.  Implement `edge_schema.rs` with `flatbuffers` builder logic.
3.  Implement `EdgeGateway` with TCP handling.
4.  Integrate `EdgeGateway` into `src/bin/server.rs`.
5.  Add tests using `std::net::TcpStream` or `tokio::net::TcpStream`.

## 5. Pros/Cons
- **Pros**: Clear separation of concerns, high performance (TCP+FlatBuffers), Raft consistency.
- **Cons**: Manual FlatBuffers implementation is verbose (due to missing `flatc`).
