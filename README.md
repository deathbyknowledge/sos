# Sea of Simulation (SoS)

![sos.png](sos.png)

> A service to manage sandboxed containers for shell agents, built with Rust and Docker.

[![Cargo](https://img.shields.io/crates/v/sos)](https://crates.io/crates/sos)
[![Rust](https://img.shields.io/badge/rust-1.82%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**SoS** provides a powerful HTTP API and CLI tool for managing isolated containerized environments where shell agents can safely execute commands. It's designed for AI/ML workloads, automated testing, and secure command execution scenarios.

## ✨ Features

- **🔌 HTTP API Server**: RESTful API for programmatic sandbox management
- **🖥️ CLI & TUI Clients**: Full-featured command-line and terminal user interface
- **⚡ Concurrent Management**: Configurable concurrency limits with semaphore-based resource control
- **🔄 Session Persistence**: Commands execute in the same bash session, preserving state
- **🧹 Automatic Cleanup**: Containers are properly stopped and removed when done
- **📊 Trajectory Tracking**: Complete history of commands executed in each sandbox
- **🎯 Standalone Mode**: Execute commands outside the session for isolated operations
- **🐳 Docker Integration**: Leverages Docker for container isolation and management
- **⏱️ Timeout Control**: Configurable sandbox timeouts for resource management

## 🎯 Use Cases

- **AI/ML Shell Agents**: Provide safe, isolated environments for language models to execute shell commands
- **Automated Testing**: Run tests in isolated, reproducible environments
- **CI/CD Pipelines**: Execute build and deployment steps in clean containers
- **Security Research**: Safely execute untrusted commands in sandboxed environments
- **Education & Training**: Provide controlled Linux environments for learning

## 📋 Prerequisites

- **Docker**: SoS requires Docker to be installed and running
  - [Install Docker](https://docs.docker.com/get-docker/)
  - Ensure your user has permissions to run Docker commands
- **Rust** (for building from source): [Install Rust](https://www.rust-lang.org/tools/install)

## 🚀 Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/deathbyknowledge/sos.git
cd sos

# Build in release mode
cargo build --release

# The binary will be available at target/release/sos
```

### Using Install Script

⚠️ **Note**: Always review scripts before running them with sudo privileges.

```bash
curl https://raw.githubusercontent.com/deathbyknowledge/sos/refs/heads/main/scripts/install.sh | sudo bash
```

### From Cargo (when published)

```bash
cargo install sos
```

## 📖 Usage

### Server Mode

Start the SoS server to manage sandboxes via HTTP API:

```bash
# Start with default settings (port 3000, max 10 sandboxes)
sos serve

# Custom configuration
sos serve --port 8080 --max-sandboxes 20 --timeout 600
```

**Options:**
- `-p, --port <PORT>`: Port to listen on (default: 3000)
- `-m, --max-sandboxes <COUNT>`: Maximum concurrent sandboxes (default: 10)
- `--timeout <SECONDS>`: Sandbox timeout in seconds (default: 600)

### Client Mode

The client can interact with a running SoS server:

#### Create a Sandbox

```bash
# Create with default ubuntu:latest image
sos sandbox create

# Create with custom image
sos sandbox create --image python:3.11-slim

# Create with setup commands
sos sandbox create \
  --image python:3.11-slim \
  --setup "pip install requests numpy" \
  --setup "mkdir -p /workspace"
```

#### List Sandboxes

```bash
# List all sandboxes
sos sandbox list

# List with custom server URL
sos sandbox --server http://remote-server:3000 list
```

#### Start a Sandbox

```bash
sos sandbox start <sandbox-id>
```

#### Execute Commands

```bash
# Execute in session mode (preserves state)
sos sandbox exec <sandbox-id> "cd /tmp"
sos sandbox exec <sandbox-id> "echo \$PWD"  # Output: /tmp
sos sandbox exec <sandbox-id> "echo 'test' > file.txt"
sos sandbox exec <sandbox-id> "cat file.txt"  # Output: test

# Execute in standalone mode (isolated process)
sos sandbox exec -s <sandbox-id> "ls -la /tmp"
```

#### Get Sandbox Trajectory

```bash
# Get JSON trajectory
curl http://localhost:3000/sandboxes/<id>/trajectory

# Get formatted trajectory
curl http://localhost:3000/sandboxes/<id>/trajectory/formatted
```

#### Stop a Sandbox

```bash
# Stop (keep metadata)
sos sandbox stop <sandbox-id>

# Stop and remove completely
curl -X POST http://localhost:3000/sandboxes/<id>/stop \
  -H "Content-Type: application/json" \
  -d '{"remove": true}'
```

#### Interactive Session Mode

The `session` command provides a REPL-like experience:

```bash
# Start an interactive session
sos session -i ubuntu:latest

# With setup commands
sos session -i python:3.11-slim --setup "pip install pandas"
```

In session mode, you can type commands interactively and see output immediately. The session maintains state across commands.

#### Terminal User Interface (TUI)

SoS includes a full-featured TUI for easier debugging and management:

```bash
# Launch TUI
sos tui

# With custom server
sos tui --server http://localhost:3000
```

The TUI provides:
- Visual sandbox management
- Real-time status monitoring
- Interactive command execution
- Trajectory viewing

## 🔄 Complete Workflow Example

```bash
# Terminal 1: Start the server
sos serve --port 3000 --max-sandboxes 5

# Terminal 2: Use the client

# Create a sandbox
ID=$(sos sandbox create --image python:3.11-slim | grep "Sandbox created" | awk '{print $NF}')
echo "Created sandbox: $ID"

# Start the sandbox
sos sandbox start $ID

# Execute commands (session is persistent)
sos sandbox exec $ID "cd /tmp"
sos sandbox exec $ID "python3 --version"
sos sandbox exec $ID "echo 'print(\"Hello\")' > hello.py"

# Run Python file
sos sandbox exec $ID "python3 hello.py"  # Output: Hello

# Check file exists (session preserved cd /tmp)
sos sandbox exec $ID "ls hello.py"

# Run standalone command (doesn't affect session)
sos sandbox exec -s $ID "ls /root"

# Stop the sandbox
sos sandbox stop $ID
```

## 🔌 HTTP API Reference

When running in server mode, the following endpoints are available:

### Create Sandbox

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

### List Sandboxes

```http
GET /sandboxes
```

**Response:**
```json
[
  {
    "id": "550e8400-e29b-41d4-a716-446655440000",
    "image": "ubuntu:latest",
    "setup_commands": "",
    "status": "Started",
    "session_command_count": 5,
    "last_standalone_exit_code": 0
  }
]
```

### Start Sandbox

```http
POST /sandboxes/{id}/start
```

**Response:** `200 OK` (empty body)

### Execute Command

```http
POST /sandboxes/{id}/exec
Content-Type: application/json

{
  "command": "echo 'Hello, World!'",
  "standalone": false
}
```

**Response:**
```json
{
  "output": "Hello, World!\n",
  "exit_code": 0,
  "exited": false
}
```

### Get Trajectory

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
      "timestamp": 0.0,
      "result": {
        "output": "",
        "exit_code": 0
      }
    }
  ]
}
```

### Get Formatted Trajectory

```http
GET /sandboxes/{id}/trajectory/formatted
```

**Response:** Plain text formatted command history

### Stop Sandbox

```http
POST /sandboxes/{id}/stop
Content-Type: application/json

{
  "remove": false
}
```

**Response:** `200 OK` (empty body)

## 🏗️ Architecture

SoS is built with the following architecture:

- **HTTP Server** (`src/lib/http.rs`): Axum-based REST API
- **Sandbox Manager** (`src/lib/sandbox/mod.rs`): Core sandbox lifecycle management
- **Shell Integration** (`src/lib/sandbox/shell.rs`): Bash session handling and command execution
- **I/O Handling** (`src/lib/sandbox/io.rs`): Stream processing and marker-based synchronization
- **CLI Client** (`src/cli/main.rs`): Command-line interface
- **TUI Client** (`src/cli/tui.rs`): Terminal user interface with Ratatui

## 🧪 Testing

### Run Tests

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_name
```

### Run Benchmarks

```bash
# Run performance benchmarks
cargo bench

# Generate HTML report
cargo bench -- --save-baseline main
```

## 🐛 Troubleshooting

### Docker Connection Issues

If you see "Failed to connect to docker":

1. Ensure Docker is running: `docker ps`
2. Check your user has Docker permissions
3. On Linux, you may need to: `sudo usermod -aG docker $USER`

### Container Startup Failures

If containers fail to start:

1. Check if the image exists locally: `docker images`
2. Verify image name is correct
3. Check Docker logs: `docker logs <container-id>`
4. Ensure sufficient disk space

### Timeout Errors

If commands timeout:

1. Increase the timeout: `sos serve --timeout 1200`
2. Check if commands are waiting for input
3. Use standalone mode for long-running processes

### Port Already in Use

If port 3000 is occupied:

```bash
# Use a different port
sos serve --port 8080

# Or specify the port in client commands
sos sandbox --server http://localhost:8080 create
```

## 📚 Examples

See the `examples/` directory for sample integrations:

- **`examples/rl/`**: Reinforcement learning integration with shell agents
- **`examples/synthetic_generator/`**: Synthetic task generation for testing

### RL Integration

The `examples/rl/` directory shows how to use SoS for training shell agents:

```bash
cd examples/rl

# Install dependencies
uv sync

# Run training
uv run train --model <model-name>

# Run benchmarks
uv run benchmark
```

Environment variables:
- `EPHEMERAL=1`: Delete sandboxes after use (set to `0` to inspect)
- `MAX_TURNS=30`: Maximum turns per rollout
- `MAX_MODEL_TOKENS=32000`: Token limit for model
- `SHELLM=1`: Whether model follows SHELLM format

## 🤝 Contributing

Contributions are welcome! Please follow these guidelines:

1. Fork the repository
2. Create a feature branch: `git checkout -b feature/amazing-feature`
3. Make your changes with tests
4. Ensure all tests pass: `cargo test`
5. Format code: `cargo fmt`
6. Commit your changes: `git commit -m "Add amazing feature"`
7. Push to the branch: `git push origin feature/amazing-feature`
8. Open a Pull Request

### Development Setup

```bash
# Clone the repo
git clone https://github.com/deathbyknowledge/sos.git
cd sos

# Install development dependencies
cargo build

# Run tests
cargo test

# Check formatting
cargo fmt --check

# Run linter
cargo clippy
```

## 📝 License

This project is licensed under the MIT License - see the LICENSE file for details.

## 🙏 Acknowledgments

- Built with [Rust](https://www.rust-lang.org/)
- Powered by [Docker](https://www.docker.com/)
- Web framework by [Axum](https://github.com/tokio-rs/axum)
- TUI by [Ratatui](https://github.com/ratatui-org/ratatui)

## 📞 Support

- 📖 [Documentation](https://github.com/deathbyknowledge/sos)
- 🐛 [Issue Tracker](https://github.com/deathbyknowledge/sos/issues)
- 💬 [Discussions](https://github.com/deathbyknowledge/sos/discussions)

---

Made with ❤️ by [deathbyknowledge](https://github.com/deathbyknowledge)
