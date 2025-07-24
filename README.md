# Sea of Simualtion (SoS)
A service to manage sandboxed containers for shell agents.

![sos.png](sos.png)
## Features

- **Server Mode**: Run an HTTP API server for managing sandboxes
- **Client Mode**: CLI client for interacting with sandbox servers
- **Concurrent Sandbox Management**: Configurable concurrency control
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
sos serve

# Custom port and concurrency limit
sos serve --port 8080 --max-sandboxes 5
```

### Client Mode

The client can interact with a running server:

#### Create a Sandbox

```bash
# Create with default ubuntu:latest image
sos sandbox create

# Create with custom image and setup commands
sos sandbox create \
  --image python:3.9 \
  --setup "pip install requests" \
  --setup "cd /workspace"
```

#### Start a Sandbox

```bash
sos sandbox start <sandbox-id>
```

#### Execute Commands

```bash
sos sandbox exec <sandbox-id> "echo 'Hello, World!'"
sos sandbox exec <sandbox-id> "ls -la"
sos sandbox exec <sandbox-id> "cd /tmp && pwd"
```

#### Stop a Sandbox

```bash
sos sandbox stop <sandbox-id>
```

#### Session Helper
Use the `session` helper enter REPL-like terminal in the sandbox
```
sos session -i ubuntu:latest
```

#### Custom Server URL

```bash
sos sandbox --server http://remote-server:3000 create
```

## Complete Workflow Example

```bash
# Terminal 1: Start the server
sos serve --port 3000

# Terminal 2: Use the client
# Create a sandbox
ID=$(sos sandbox create --image ubuntu:latest | grep "Sandbox created" | cut -d' ' -f5)

# Start the sandbox
sos sandbox start $ID

# Execute commands (session is persistent)
sos sandbox exec $ID "cd /tmp"
sos sandbox exec $ID "echo \$PWD"  # Should output: /tmp
sos sandbox exec $ID "echo 'Hello World' > test.txt"
sos sandbox exec $ID "cat test.txt"

# Clean up
sos sandbox stop $ID
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
cargo test
```

Run benchmarks:

```bash
cargo bench
```