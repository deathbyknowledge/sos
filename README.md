# Sea of Simulation (SoS) v0.0.7

A service to manage sandboxed containers for shell agents.

![sos.png](sos.png)

## Features

- **Server Mode**: Run an HTTP API server for managing sandboxes
- **CLI/TUI Mode**: CLI and TUI clients for interacting with sandbox servers
- **Concurrent Sandbox Management**: Configurable concurrency control with semaphore-based limiting
- **Session Persistence**: Commands executed in the same bash session
- **Automatic Cleanup**: Containers are properly stopped and removed
- **Timeout Management**: Automatic sandbox timeout after configurable inactivity period

## Installation

### From source

```bash
cargo build --release
```

The binary will be available at `target/release/sos`.

### From release binary

(Read the script before blindly installing it)

```bash
curl https://raw.githubusercontent.com/deathbyknowledge/sos/refs/heads/main/scripts/install.sh | sudo bash
```

## Usage

### Server Mode

Start the sandbox server:

```bash
# Start server on default port 3000 with max 10 concurrent sandboxes and 10 minute timeout
sos serve

# Custom port and concurrency limit
sos serve --port 8080 --max-sandboxes 20

# Custom timeout (in seconds)
sos serve --timeout 300  # 5 minutes
```

**Server Options:**
- `-p, --port PORT` - Port to listen on (default: 3000)
- `-m, --max-sandboxes NUM` - Maximum concurrent sandboxes (default: 10)
- `--timeout SECONDS` - Sandbox timeout in seconds (default: 600)

The timeout determines how long a sandbox can remain active before being automatically stopped.

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

# Multiple setup commands are supported - they will be chained with &&
```

**Create Options:**
- `-i, --image IMAGE` - Container image to use (default: ubuntu:latest)
- `-s, --setup CMD` - Setup commands to run after container start (can be specified multiple times)

#### List Sandboxes

```bash
# List all sandboxes
sos sandbox list
```

This shows the sandbox ID, image, status, and setup commands for each sandbox.

#### Start a Sandbox

```bash
sos sandbox start <sandbox-id>
```

#### Execute Commands

```bash
# Execute in the persistent session
sos sandbox exec <sandbox-id> "echo 'Hello, World!'"
sos sandbox exec <sandbox-id> "ls -la"
sos sandbox exec <sandbox-id> "cd /tmp && pwd"

# Execute in standalone mode (outside the session)
sos sandbox exec -s <sandbox-id> "cat /tmp/test.txt"
sos sandbox exec --standalone <sandbox-id> "env"
```

**Exec Options:**
- `-s, --standalone` - Run command in standalone mode (new process, not in session)

#### View Trajectory

```bash
# View raw JSON trajectory
sos sandbox trajectory <sandbox-id>

# View human-readable formatted trajectory
sos sandbox trajectory <sandbox-id> --formatted
```

**Trajectory Options:**
- `-f, --formatted` - Format output as human-readable text

#### Stop a Sandbox

```bash
# Stop sandbox but keep it registered
sos sandbox stop <sandbox-id>

# Stop and remove sandbox from the server
sos sandbox stop <sandbox-id> --remove
```

**Stop Options:**
- `-r, --remove` - Remove sandbox from server after stopping

#### Session Helper

Use the `session` helper to enter a REPL-like terminal in the sandbox:

```bash
# Start interactive session with default image
sos session

# Start with specific image
sos session --image python:3.11

# Start with setup commands
sos session --image ubuntu:latest --setup "apt-get update" --setup "apt-get install -y curl"
```

**Session Options:**
- `-s, --server URL` - Server URL (default: http://localhost:3000)
- `-i, --image IMAGE` - Container image to use (default: ubuntu:latest)
- `--setup CMD` - Setup commands to run after container start (can be specified multiple times)

#### Custom Server URL

```bash
# Use a remote server for all sandbox commands
sos sandbox --server http://remote-server:3000 create --image python:3.9
```

## Complete Workflow Examples

### Basic Workflow

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

# Verify file exists using standalone mode
sos sandbox exec -s $ID "cat /tmp/test.txt"

# View the command trajectory
sos sandbox trajectory $ID --formatted

# Clean up - stop and remove
sos sandbox stop $ID --remove
```

### Python Development Sandbox

```bash
# Create with Python image and setup
sos sandbox create \
  --image python:3.11 \
  --setup "pip install pytest requests numpy" \
  --setup "mkdir -p /workspace/app"

ID=$(sos sandbox create --image python:3.11 | grep -oP 'ID: \K[^\s]+')
sos sandbox start $ID

# Run Python commands
sos sandbox exec $ID "python --version"
sos sandbox exec $ID "pip list"

# View trajectory to see all commands
sos sandbox trajectory $ID --formatted
```

### Interactive Session

```bash
# Start server
sos serve

# In another terminal, start interactive session
sos session --image node:20 --setup "npm install -g pnpm"

# Now you're in an interactive shell:
# sandbox:abc12345> node --version
# sandbox:abc12345> mkdir -p /app
# sandbox:abc12345> cd /app
# sandbox:abc12345> exit  # This cleans up the sandbox automatically
```

## TUI

The client also includes a complete TUI version for easier debugging and use:

```bash
# Connect to default server
sos tui

# Connect to custom server
sos tui --server http://remote-server:3000
```

The TUI provides a visual interface for:
- Viewing all sandboxes and their statuses
- Creating and stopping sandboxes
- Executing commands
- Viewing trajectories in real-time

## HTTP API

When running in server mode, the following endpoints are available:

### Create Sandbox

**POST** `/sandboxes`

Create a new sandbox without starting the container.

**Request Body:**
```json
{
  "image": "ubuntu:latest",
  "setup_commands": ["apt-get update", "apt-get install -y curl"]
}
```

**Response:**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000"
}
```

**Parameters:**
- `image` (string) - Docker image to use (default: ubuntu:latest)
- `setup_commands` (array of strings) - Commands to run after container start, chained with `&&`

### List Sandboxes

**GET** `/sandboxes`

List all existing sandboxes.

**Response:**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "image": "ubuntu:latest",
    "setup_commands": "apt-get update && apt-get install -y curl",
    "status": "started",
    "session_command_count": 5,
    "last_standalone_exit_code": null
  }
]
```

**SandboxInfo Fields:**
- `id` (string) - Unique identifier for the sandbox (UUID)
- `image` (string) - Docker image used
- `setup_commands` (string) - Chained setup commands
- `status` (string) - Current status: `created`, `started`, `exited`, or `stopped`
- `session_command_count` (number) - Number of commands executed in the session
- `last_standalone_exit_code` (number or null) - Exit code of the last standalone command

### Start Sandbox

**POST** `/sandboxes/{id}/start`

Start a sandbox with the given ID and run the setup commands. This acquires a permit from the semaphore, limiting concurrent sandboxes.

**Parameters:**
- `id` (path) - Sandbox ID

### Execute Command

**POST** `/sandboxes/{id}/exec`

Execute a command in the sandbox.

**Request Body:**
```json
{
  "command": "ls -la",
  "standalone": false
}
```

**Response:**
```json
{
  "output": "total 24\ndrwxr-xr-x 2 root root 4096 ...",
  "exit_code": 0,
  "exited": false
}
```

**Parameters:**
- `command` (string) - Command to execute
- `standalone` (boolean) - If true, run as a new process; if false, run in the existing session (default: false)

**CommandResult Fields:**
- `output` (string) - Combined stdout and stderr
- `exit_code` (number) - Exit code of the command
- `exited` (boolean) - Whether the command caused the session to exit

### Stop Sandbox

**POST** `/sandboxes/{id}/stop`

Stop a sandbox with the given ID.

**Request Body:**
```json
{
  "remove": true
}
```

**Parameters:**
- `id` (path) - Sandbox ID
- `remove` (boolean) - If true, remove sandbox from server after stopping; if false, keep it registered (default: false)

### Get Trajectory

**GET** `/sandboxes/{id}/trajectory`

Get the command trajectory of a sandbox in JSON format.

**Response:**
```json
{
  "sandbox_id": "550e8400-e29b-41d4-a716-446655440000",
  "command_count": 3,
  "trajectory": [
    {
      "index": 0,
      "command": "cd /tmp",
      "timestamp": 0.523
    },
    {
      "index": 1,
      "command": "echo $PWD",
      "timestamp": 1.234,
      "result": {
        "output": "/tmp\n",
        "exit_code": 0
      }
    }
  ]
}
```

The `timestamp` field is relative to when the sandbox was started (in seconds).

### Get Formatted Trajectory

**GET** `/sandboxes/{id}/trajectory/formatted`

Get the command trajectory in a human-readable text format.

**Response (text/plain):**
```
$ cd /tmp
/tmp

$ echo $PWD
/tmp

$ ls -la
total 24
drwxr-xr-x 2 root root 4096 ...
```

## Testing

Run the integration tests:

```bash
cargo test
```

Run benchmarks:

```bash
cargo bench
```

## How It Works

### Session Mode vs Standalone Mode

- **Session Mode (default)**: Commands are executed in a persistent bash session. State changes (e.g., `cd /tmp`) persist across commands.
- **Standalone Mode (`-s` flag)**: Each command runs in a fresh process. State does not persist. Useful for verification commands.

### Concurrency Control

The server uses a semaphore to limit concurrent sandbox executions. When the limit is reached, new sandbox starts will block until a permit becomes available.

### Timeout

Sandboxes are automatically stopped after a configurable timeout period (default: 10 minutes). This prevents runaway containers from consuming resources indefinitely.

### Setup Commands

Setup commands are chained with `&&` and executed in standalone mode immediately after the container starts. If any setup command fails, the sandbox fails to start.

## License

[Add your license here]
