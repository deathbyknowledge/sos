# Sea of Simulation (SoS)

[![CI](https://github.com/deathbyknowledge/sos/actions/workflows/ci.yml/badge.svg)](https://github.com/deathbyknowledge/sos/actions/workflows/ci.yml)

A high-performance service for managing sandboxed containers, designed specifically for shell agents and AI-assisted command execution.

![sos.png](sos.png)

## Overview

SoS provides a robust HTTP API server and CLI/TUI client suite for managing isolated Docker containers. It's ideal for:

- **AI/ML Applications**: Safe execution of shell commands by language models
- **Testing & CI/CD**: Isolated test environments with automatic cleanup
- **Research**: Reproducible experimental setups with trajectory tracking
- **Development**: On-demand sandboxed environments with full session persistence

## Features

### Core Capabilities
- **Server Mode**: Full-featured HTTP API for managing sandboxes programmatically
- **CLI Mode**: Command-line client for manual sandbox management
- **TUI Mode**: Interactive terminal UI with mouse support and keyboard navigation
- **Concurrent Sandbox Management**: Configurable concurrency limits with semaphore-based control
- **Session Persistence**: All commands execute in the same bash session, maintaining state
- **Trajectory Tracking**: Complete command history with timestamps and results
- **Automatic Cleanup**: Containers are properly stopped and removed after use
- **Timeout Management**: Configurable sandbox timeouts with automatic reclamation
- **Standalone Execution**: Run commands in isolated processes when needed

### Advanced Features
- **Setup Commands**: Automatically run initialization commands on container start
- **Custom Images**: Use any Docker image as the base for your sandboxes
- **Session Helper**: REPL-like interactive sessions with `sos session`
- **Formatted Output**: Human-readable or JSON-formatted trajectory output
- **Cross-Platform Support**: Linux (x86_64, ARM64) and macOS (x86_64, ARM64)

## Installation

### Prerequisites

- Docker or Podman running locally
- Rust toolchain (if building from source)
- For the TUI: A terminal that supports mouse and alternate screen mode

### From Source

```bash
# Clone the repository
git clone https://github.com/deathbyknowledge/sos.git
cd sos

# Build the release binary
cargo build --release

# The binary will be at target/release/sos
```

### From Release Binary

```bash
# Install directly (review the script before running)
curl https://raw.githubusercontent.com/deathbyknowledge/sos/main/scripts/install.sh | sudo bash
```

Pre-built binaries are available on the [Releases](https://github.com/deathbyknowledge/sos/releases) page for:
- Linux (x86_64, aarch64)
- macOS (x86_64, aarch64)

### Using Cargo

```bash
cargo install sos
```

## Usage

### Quick Start

```bash
# Terminal 1: Start the server
sos serve

# Terminal 2: Create and use a sandbox
sos sandbox create --image ubuntu:latest
sos sandbox start $(sos sandbox list | head -1 | awk '{print $1}')
sos sandbox exec <id> "echo 'Hello, World!'"
```

### Server Mode

Start the SoS server with configurable options:

```bash
# Start with defaults (port 3000, max 10 sandboxes, 10 min timeout)
sos serve

# Custom configuration
sos serve --port 8080 --max-sandboxes 20 --timeout 600
```

**Options:**
- `-p, --port <PORT>`: HTTP server port (default: 3000)
- `-m, --max-sandboxes <N>`: Maximum concurrent sandboxes (default: 10)
- `--timeout <SECONDS>`: Sandbox auto-stop timeout (default: 600)

### CLI Mode

#### Create a Sandbox

```bash
# Basic creation with default image
sos sandbox create

# With custom image and setup commands
sos sandbox create \
  --image python:3.11 \
  --setup "pip install requests numpy" \
  --setup "mkdir -p /workspace"
```

#### List Sandboxes

```bash
sos sandbox list
```

Output format:
```
ID                                    IMAGE              STATUS    CMDS  EXIT  SETUP
550e8400-e29b-41d4-a716-446655440000   ubuntu:latest      Created   0    N/A   none
```

#### Start a Sandbox

```bash
sos sandbox start <sandbox-id>
```

#### Execute Commands

```bash
# Execute in the session (persistent shell state)
sos sandbox exec <id> "cd /tmp"
sos sandbox exec <id> "echo $PWD"  # Outputs: /tmp
sos sandbox exec <id> "echo 'Hello' > test.txt"
sos sandbox exec <id> "cat test.txt"  # Outputs: Hello

# Execute standalone (fresh shell process)
sos sandbox exec -s <id> "pwd"  # Outputs: /root (or default working dir)
```

#### View Trajectory

```bash
# JSON format (for programmatic access)
sos sandbox trajectory <id>

# Human-readable format
sos sandbox trajectory <id> --formatted
```

Example formatted output:
```
[0.001s] $ echo 'Hello, World!'
Hello, World!

[1.234s] $ pwd
/tmp

[2.456s] $ echo 'Done' > output.txt
```

#### Stop a Sandbox

```bash
# Stop but keep in server (for trajectory inspection)
sos sandbox stop <id> --remove false

# Stop and completely remove
sos sandbox stop <id>
```

### Interactive Session Mode

The `session` command provides a REPL-like interface for easier debugging:

```bash
# Basic session
sos session -i ubuntu:latest

# With setup commands
sos session \
  -i python:3.11 \
  --setup "pip install requests" \
  --setup "cd /workspace"
```

Session commands:
- Type any shell command and press Enter to execute
- Type `exit` or `quit` to exit (auto-cleanup)
- Session persists shell state (variables, working directory, etc.)

### TUI Mode

The Terminal User Interface provides a full-featured interactive experience:

```bash
# Connect to local server
sos tui

# Connect to remote server
sos tui --server http://remote:3000
```

**TUI Screens & Features:**

1. **Sandbox List** (default view)
   - View all sandboxes with status
   - Scroll with `j`/`k` or arrow keys
   - Press `Enter` to view details
   - Press `n` to create new sandbox
   - Press `r` to refresh list

2. **Sandbox Detail View**
   - View complete trajectory
   - Press `t` to toggle between formatted/JSON
   - Press `s` to start interactive session
   - Press `x` to stop and remove sandbox

3. **Interactive Session**
   - Execute commands in sandbox
   - View output in real-time
   - Scroll through history while typing is idle

4. **New Sandbox Wizard**
   - Step-by-step sandbox creation
   - Enter Docker image
   - Add setup commands one-by-one

**TUI Keyboard Shortcuts:**

| Key | Action |
|-----|--------|
| `j`/`↓` | Scroll down |
| `k`/`↑` | Scroll up |
| `gg` | Jump to top |
| `G` | Jump to bottom |
| `Ctrl-U` | Scroll up half page |
| `Ctrl-D` | Scroll down half page |
| `Enter` | Select / Submit |
| `Esc` | Go back / Cancel |
| `q` | Quit (on main screen) |
| `r` | Refresh list |
| `n` | New sandbox |
| `s` | Start session |
| `x` | Stop & remove |
| `t` | Toggle trajectory format |
| `F1` | Toggle mouse mode |
| `Ctrl-C` | Copy content to clipboard |

## HTTP API

SoS exposes a RESTful HTTP API when running in server mode.

### Base URL
```
http://localhost:3000
```

### Endpoints

#### Create Sandbox
```http
POST /sandboxes
Content-Type: application/json

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

#### List Sandboxes
```http
GET /sandboxes
```

**Response:**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "image": "ubuntu:latest",
    "setup_commands": "apt-get update && apt-get install -y curl",
    "status": "Started",
    "session_command_count": 5,
    "last_standalone_exit_code": 0
  }
]
```

#### Start Sandbox
```http
POST /sandboxes/{id}/start
```

**Response:** `204 No Content` on success

#### Execute Command
```http
POST /sandboxes/{id}/exec
Content-Type: application/json

{
  "command": "ls -la",
  "standalone": false
}
```

**Response:**
```json
{
  "output": "total 24\ndrwxr-xr-x 2 root root 4096 ...\n",
  "exit_code": 0,
  "exited": false
}
```

#### Get Trajectory
```http
GET /sandboxes/{id}/trajectory
```

**Response:**
```json
{
  "sandbox_id": "550e8400-e29b-41d4-a716-446655440000",
  "command_count": 3,
  "trajectory": [
    {
      "index": 0,
      "command": "cd /tmp",
      "timestamp": 0.123,
      "result": {
        "output": "",
        "exit_code": 0
      }
    },
    {
      "index": 1,
      "command": "echo 'Hello'",
      "timestamp": 0.456,
      "result": {
        "output": "Hello\n",
        "exit_code": 0
      }
    }
  ]
}
```

#### Get Formatted Trajectory
```http
GET /sandboxes/{id}/trajectory/formatted
```

**Response:** `text/plain` with formatted output

#### Stop Sandbox
```http
POST /sandboxes/{id}/stop
Content-Type: application/json

{
  "remove": true
}
```

**Response:** `204 No Content` on success

### Error Responses

All endpoints return appropriate HTTP status codes:
- `400 Bad Request` - Invalid parameters or sandbox state
- `404 Not Found` - Sandbox doesn't exist
- `504 Gateway Timeout` - Command execution timeout
- `500 Internal Server Error` - Server error

## Python Client

A Python client library is provided for easy integration:

```python
import asyncio
from sos import SoSClient

async def main():
    client = SoSClient(server_url="http://localhost:3000")
    
    # Create a sandbox with setup commands
    sandbox_id = await client.create_sandbox(
        image="python:3.11",
        setup_commands=["pip install requests", "mkdir -p /workspace"]
    )
    
    try:
        # Start the sandbox
        await client.start_sandbox(sandbox_id)
        
        # Execute commands
        output, exit_code, exited = await client.exec_command(
            sandbox_id,
            "python -c 'print(\"Hello from sandbox!\")'"
        )
        print(output)
        
        # Get trajectory
        trajectory = await client.get_sandbox_trajectory(sandbox_id, formatted=True)
        print(trajectory)
        
    finally:
        # Cleanup
        await client.stop_sandbox(sandbox_id, remove=True)

asyncio.run(main())
```

See `examples/rl/sos.py` for the full client implementation.

## Examples

### Reinforcement Learning Training

The repository includes an example for training RL agents with shell environments:

```bash
cd examples/rl

# Ensure sos serve is running, then:
uv run train --model <model-name>
uv run benchmark
```

Environment variables:
- `EPHEMERAL=1` - Delete sandboxes after use (0 to inspect)
- `MAX_TURNS=30` - Maximum turns per rollout
- `MAX_MODEL_TOKENS=32000` - Token limit for model responses
- `SHELLM=1` - Use SHELLM format instead of standard chat

See `examples/rl/README.md` for details.

### Synthetic Data Generation

```bash
cd examples/synthetic_generator
python generation.py
```

## Development

### Project Structure

```
sos/
├── src/
│   ├── cli/           # Command-line interface
│   │   ├── main.rs    # CLI entry point
│   │   └── tui.rs     # Terminal UI implementation
│   └── lib/           # Core library
│       ├── mod.rs     # Library entry point
│       ├── http.rs    # HTTP API server
│       └── sandbox/   # Docker container management
├── tests/             # Integration tests
├── benches/           # Performance benchmarks
└── examples/          # Usage examples
```

### Running Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_name
```

### Running Benchmarks

```bash
# Run performance benchmarks
cargo bench

# View HTML report
open target/criterion/report/index.html
```

### Building for Different Targets

```bash
# Linux ARM64
cargo build --release --target aarch64-unknown-linux-gnu

# macOS Apple Silicon
cargo build --release --target aarch64-apple-darwin

# macOS Intel
cargo build --release --target x86_64-apple-darwin
```

### Docker Integration

SoS works with both Docker and Podman. The client automatically detects which one to use:

- Docker: Uses default socket `/var/run/docker.sock`
- Podman: Uses `podman.sock` when available

### Logging

SoS uses `tracing` for structured logging:

```bash
# Default logging (info for sos, warn for dependencies)
sos serve

# Debug logging
RUST_LOG=debug sos serve

# JSON logging for log aggregation
RUST_LOG=sos=info sos serve 2>&1 | jq
```

### Environment Variables

- `RUST_LOG` - Log level filter (e.g., `info`, `debug`, `warn`)
- `DOCKER_HOST` - Docker daemon socket URL (default: auto-detected)

## Configuration

### Server Configuration

All server options can be passed via CLI flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `3000` | HTTP server port |
| `--max-sandboxes` | `10` | Maximum concurrent sandboxes |
| `--timeout` | `600` | Sandbox timeout in seconds |

### Client Configuration

Specify custom server URL:

```bash
# Per command
sos sandbox --server http://remote:3000 create

# Set environment variable for all commands
export SOS_SERVER=http://remote:3000
sos sandbox create  # Uses $SOS_SERVER if set
```

## Architecture

### Sandbox Lifecycle

```
1. Create → Register sandbox, no container started
2. Start   → Pull image (if needed), create and start container
3. Exec    → Execute commands in the container
4. Stop    → Stop and optionally remove container
```

### Concurrency Control

- Server uses a semaphore to limit concurrent sandbox executions
- Each running sandbox holds a permit
- When sandbox stops, permit is released
- Waiting requests are queued until permits available

### Session vs Standalone Execution

- **Session Mode**: Commands share bash process (variables, working directory persist)
- **Standalone Mode**: Each command runs in fresh shell (isolated environment)

Example:
```bash
# Session mode (default)
exec "export MY_VAR=hello"
exec "echo $MY_VAR"  # Output: hello

# Standalone mode
exec -s "export MY_VAR=hello"
exec -s "echo $MY_VAR"  # Output: (empty)
```

## Troubleshooting

### Docker Connection Issues

```bash
# Check Docker is running
docker ps

# Check socket permissions
ls -la /var/run/docker.sock
```

### Permission Denied

```bash
# Add user to docker group (Linux)
sudo usermod -aG docker $USER
newgrp docker
```

### Port Already in Use

```bash
# Use a different port
sos serve --port 3001

# Or kill the process using port 3000
lsof -ti:3000 | xargs kill
```

### Image Pull Failures

```bash
# Pre-pull images
docker pull ubuntu:latest
docker pull python:3.11

# Check Docker Hub connectivity
ping registry-1.docker.io
```

## Contributing

Contributions are welcome! Please read our guidelines:

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes with tests
4. Ensure all tests pass (`cargo test`)
5. Commit with conventional commits (`feat: add amazing feature`)
6. Push to your branch (`git push origin feature/amazing-feature`)
7. Open a Pull Request

### Development Setup

```bash
# Install pre-commit hooks (if available)
cargo install cargo-hack

# Run formatting check
cargo fmt -- --check

# Run clippy
cargo clippy -- -D warnings
```

## License

This project is released under the MIT License. See LICENSE file for details.

## Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- Container management via [Bollard](https://github.com/fussybeaver/bollard)
- Web framework: [Axum](https://github.com/tokio-rs/axum)
- TUI: [Ratatui](https://github.com/ratatui-org/ratatui)

## Support

- **Issues**: [GitHub Issues](https://github.com/deathbyknowledge/sos/issues)
- **Discussions**: [GitHub Discussions](https://github.com/deathbyknowledge/sos/discussions)

---

**Sea of Simulation** - Making safe shell execution accessible.
