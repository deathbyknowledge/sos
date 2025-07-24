# Grid
A service to manage sandboxed containers for shell agents.

## Features

- **Server Mode**: Run an HTTP API server for managing sandboxes
- **Client Mode**: CLI client for interacting with sandbox servers
- **Concurrent Sandbox Management**: Configurable semaphore-based concurrency control
- **Session Persistence**: Commands executed in the same bash session
- **Automatic Cleanup**: Containers are properly stopped and removed

## Installation

```bash
cargo build --release
```

## Usage

### Server Mode

Start the sandbox server:

```bash
# Start server on default port 3000 with max 10 concurrent sandboxes
./target/release/runtime serve

# Custom port and concurrency limit
./target/release/runtime serve --port 8080 --max-sandboxes 5
```

### Client Mode

The client can interact with a running server:

#### Create a Sandbox

```bash
# Create with default ubuntu:latest image
./target/release/runtime sandbox create

# Create with custom image and setup commands
./target/release/runtime sandbox create \
  --image python:3.9 \
  --setup "pip install requests" \
  --setup "cd /workspace"
```

#### Start a Sandbox

```bash
./target/release/runtime sandbox start <sandbox-id>
```

#### Execute Commands

```bash
./target/release/runtime sandbox exec <sandbox-id> "echo 'Hello, World!'"
./target/release/runtime sandbox exec <sandbox-id> "ls -la"
./target/release/runtime sandbox exec <sandbox-id> "cd /tmp && pwd"
```

#### Stop a Sandbox

```bash
./target/release/runtime sandbox stop <sandbox-id>
```

#### Custom Server URL

```bash
./target/release/runtime sandbox --server http://remote-server:3000 create
```

## Complete Workflow Example

```bash
# Terminal 1: Start the server
./target/release/runtime serve --port 3000

# Terminal 2: Use the client
# Create a sandbox
ID=$(./target/release/runtime sandbox create --image ubuntu:latest | grep "Sandbox created" | cut -d' ' -f5)

# Start the sandbox
./target/release/runtime sandbox start $ID

# Execute commands (session is persistent)
./target/release/runtime sandbox exec $ID "cd /tmp"
./target/release/runtime sandbox exec $ID "echo \$PWD"  # Should output: /tmp
./target/release/runtime sandbox exec $ID "echo 'Hello World' > test.txt"
./target/release/runtime sandbox exec $ID "cat test.txt"

# Clean up
./target/release/runtime sandbox stop $ID
```

## HTTP API

When running in server mode, the following endpoints are available:

- `POST /sandboxes` - Create a new sandbox
- `POST /sandboxes/{id}/start` - Start a sandbox
- `POST /sandboxes/{id}/exec` - Execute a command in a sandbox
- `DELETE /sandboxes/{id}` - Stop and remove a sandbox

## Testing

Run the integration tests:

```bash
cargo test --test integration_tests -- --test-threads=1
```

Run benchmarks:

```bash
cargo bench
```

## Architecture

- **Server**: Axum HTTP server with Docker/Podman backend
- **Client**: HTTP client using reqwest
- **Concurrency**: Tokio semaphore for limiting concurrent sandboxes
- **Session Management**: Persistent bash sessions using container attach
- **Command Isolation**: Commands are executed with proper output capture and cleanup 