# Vortex

A high-performance HTTP framework for Linux, built on io_uring.

## Features

- **io_uring** with multishot accept, provided buffer rings, registered files, and cascading kernel fallbacks
- **Thread-per-core** architecture with CPU pinning — no work-stealing, no shared state
- **Zero-allocation HTTP parser** with tiered fast-path classification
- **Custom PostgreSQL client** using binary wire protocol, pipelined queries, and async I/O
- **Hand-optimized JSON** serialization — direct-to-buffer with itoa, no serde
- **PGO + BOLT** support for production builds

## Quick Start

```bash
docker compose up
```

Starts the server on port 8080 with PostgreSQL.

## Build Optimized Binary

```bash
docker build -f vortex.dockerfile -t vortex .
```

Produces a PGO + BOLT optimized binary for maximum throughput.

## License

MIT
