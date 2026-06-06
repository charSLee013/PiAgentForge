# Pi - AI Coding Agent

A production-quality Rust implementation of an AI-powered coding assistant with multi-provider support, interactive TUI, and extensible tool system.

## Features

- 🤖 **Multi-Provider Support** - OpenAI, Anthropic, Google, Mistral, AWS Bedrock
- 💬 **Interactive TUI** - Full-featured terminal interface with session management
- 🔧 **Built-in Tools** - Bash, Read, Write, Edit, Find, Grep for code manipulation
- 📝 **Session Persistence** - JSONL-based storage with branching and resume support
- 🔌 **Extensible** - Optional WASM extension system for custom tools
- 🔐 **OAuth Support** - Device code flows for Anthropic, GitHub Copilot, OpenAI
- 🎯 **Plan Mode** - Read-only tool restrictions for safe exploration
- 🌐 **RPC Mode** - JSONL-over-stdio RPC mode for programmatic access

## Quick Start

### Prebuilt Binaries

GitHub Releases publishes prebuilt archives for the `pi` CLI, so most users do
not need to build from source:

- Linux: `x86_64-unknown-linux-gnu`
- macOS: `aarch64-apple-darwin`
- Windows: `x86_64-pc-windows-msvc`

Download the latest release assets from:

<https://github.com/charSLee013/PiAgentForge/releases>

Official release binaries include the default provider surface plus
`feat-extensions`. Each release also includes a `SHA256SUMS` file for asset
verification.

### Installation

```bash
# Clone the repository
git clone https://github.com/yourusername/pi-to-rust.git
cd pi-to-rust

# Build with all providers (default)
cargo build --release

# Or build with specific providers
cargo build --release --no-default-features --features feat-openai,feat-anthropic
```

### Basic Usage

```bash
# Non-interactive mode (print)
pi --print "explain this codebase"

# Interactive TUI mode
pi --interactive

# Use specific provider and model
pi --provider anthropic --model claude-sonnet-4-20250514 --print "hello"

# Use custom base URL (e.g., for DeepSeek)
pi --provider openai --model deepseek-v4-pro --base-url https://api.deepseek.com/v1 --print "hello"
```

### Configuration

Pi resolves different settings from different sources:

- **Model / provider**: CLI flags, then `~/.pi/settings.json`, then built-in defaults
- **API key**: `--api-key`, then `~/.pi/settings.json`, then provider env vars such as `OPENAI_API_KEY`, then `~/.pi/auth.json`
- **Base URL**: `--base-url`, then `PI_BASE_URL`, then `~/.pi/settings.json`

Example `~/.pi/settings.json`:

```json
{
  "provider": "anthropic",
  "model": "claude-sonnet-4-20250514",
  "api_key": "sk-ant-..."
}
```

## Build Options

### Feature Flags

Pi uses Cargo features for conditional compilation:

| Feature | Description | Default |
|---------|-------------|---------|
| `feat-all` | All providers enabled | ✅ |
| `feat-openai` | OpenAI Chat Completions API | ✅ |
| `feat-anthropic` | Anthropic Messages API | ✅ |
| `feat-google` | Google Generative AI | ✅ |
| `feat-mistral` | Mistral Conversations API | ✅ |
| `feat-bedrock` | AWS Bedrock Runtime | ✅ |
| `feat-extensions` | WASM extension system | ❌ |

### Build Examples

```bash
# Minimal build (OpenAI only)
cargo build --release --no-default-features --features feat-openai

# With Anthropic support
cargo build --release --no-default-features --features feat-openai,feat-anthropic

# All providers
cargo build --release --features feat-all

# With WASM extensions (larger binary)
cargo build --release --features feat-all,feat-extensions
```

## Usage Examples

### Interactive Mode

```bash
# Start interactive session
pi --interactive

# Resume previous session
pi --interactive --resume

# Continue last session
pi --interactive --continue

# Fork from specific session
pi --interactive --fork <session-id>
```

**Slash Commands in TUI:**
- `/model` - Switch model
- `/theme` - Change theme
- `/session` - Resume session
- `/plan` - Enter plan mode (read-only tools)
- `/subagent` - Spawn subagent
- `/extensions` - Manage WASM extensions (if enabled)

### Print Mode

```bash
# Single-shot print mode
pi --print "refactor this function to use async/await"

# With specific model
pi --provider openai --model gpt-4 --print "explain the agent loop"

# Disable tools
pi --print "analyze this code" --no-tools

# Raise the runtime turn cap (default: 200)
pi --max-turns 1000 --print "work through a long benchmark"

# Stream bash stdout/stderr while tools run
pi --stream-stdout --print "run the test suite and show progress"

# Emit JSONL agent events
pi --json --print "summarize this repository"

# Use PI_BASE_URL as the base-url fallback for OpenAI-compatible endpoints
PI_BASE_URL=https://api.deepseek.com/v1 pi --model deepseek-v4-pro --print "hello"
```

### OAuth Login

```bash
# Login to Anthropic
pi --login anthropic

# Login to GitHub Copilot
pi --login github-copilot

# Login to OpenAI Codex
pi --login openai-codex
```

### Session Management

```bash
# List available models
pi --list-models

# Export session to HTML
pi --export <session-id>

# RPC mode (JSONL over stdin/stdout)
pi --rpc
```

## Architecture

### Workspace Structure

```
pi-to-rust/
├── pi/                          # Main binary entry point
├── pi-cli/                      # CLI argument parsing & orchestration
├── pi-core/                     # Core utilities (auth, settings, tools, I/O)
├── pi-ai-core/                  # LLM abstraction layer
├── pi-agent-core/               # Agent loop, session management
├── pi-model-catalog/            # Model definitions (embedded JSON)
├── pi-tui-app/                  # Terminal UI application
├── pi-tui-core/                 # TUI primitives
├── pi-modes/                    # RPC server mode
├── pi-oauth/                    # OAuth flows
├── pi-extension-system/         # WASM extension loader
└── pi-provider-*/               # Provider implementations
```

### Core Components

**AI Layer** (`pi-ai-core`)
- Provider-agnostic LLM interface
- `ApiProvider` trait for streaming responses
- SSE stream parsing with provider-specific events

**Agent Layer** (`pi-agent-core`)
- State machine-based conversation orchestration
- Tool execution with pending call queue
- Context compaction with token estimation
- Session persistence (JSONL format)

**Tool Layer** (`pi-core/tools`)
- Built-in tools: Bash, Read, Write, Edit, Find, Grep, Ls
- File mutation queue for atomic operations
- Output truncation for large results

**UI Layer** (`pi-tui-app`)
- Component-based terminal rendering
- Model/theme/session selectors
- Plan mode and subagent workflows

## Provider Details

### OpenAI

```bash
export OPENAI_API_KEY="sk-..."
pi --provider openai --model gpt-4 --print "hello"
```

- Endpoint: `https://api.openai.com/v1/chat/completions`
- Supports reasoning content (DeepSeek-compatible)
- Tool call streaming with delta accumulation

### Anthropic

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
pi --provider anthropic --model claude-sonnet-4-20250514 --print "hello"

# Or use custom base URL
export ANTHROPIC_BASE_URL="https://custom.api.com/v1/messages"
```

- Endpoint: `https://api.anthropic.com/v1/messages` (configurable via `ANTHROPIC_BASE_URL`)
- Thinking blocks with signatures
- OAuth token detection (`sk-ant-oat`)
- Tool call ID normalization

### AWS Bedrock

```bash
# No API key required - uses AWS credentials
pi --provider bedrock --model anthropic.claude-3-sonnet-20240229-v1:0 --print "hello"
```

- Uses AWS SDK (`aws-sdk-bedrockruntime`)
- Requires AWS credentials configured
- Base64 encoding for binary payloads

### Google Generative AI

```bash
export GOOGLE_API_KEY="..."
pi --provider google --model gemini-pro --print "hello"
```

### Mistral

```bash
export MISTRAL_API_KEY="..."
pi --provider mistral --model mistral-large --print "hello"
```

## Development

### Prerequisites

- Rust 1.85+ (MSRV)
- Cargo

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for specific crate
cargo test -p pi-agent-core

# Run integration tests
cargo test --test e2e_full_long_flow
```

**Test Coverage:** 861 test functions across all crates

### Code Quality

```bash
# Format code
cargo fmt

# Lint with Clippy
cargo clippy --all-targets --all-features

# Check all feature combinations
cargo check --no-default-features --features feat-openai
cargo check --features feat-all
cargo check --features feat-all,feat-extensions
```

### Project Configuration

- **`clippy.toml`** - MSRV: 1.85
- **`rustfmt.toml`** - Max width: 120, import grouping: `StdExternalCrate`

## Advanced Features

### Context Compaction

When context exceeds model's `max_input_tokens`, Pi automatically:
1. Estimates tokens with `estimate_message_tokens()`
2. Finds cut point (keeps recent `COMPACTION_KEEP_RECENT_TOKENS`)
3. Generates summary via LLM
4. Replaces old messages with summary message

### Session Storage

Sessions are stored in `~/.pi/sessions/` as JSONL files:

```jsonl
{"type":"header","session_id":"...","cwd":"...","timestamp":...}
{"type":"message","id":"u1","parent_id":null,"timestamp":...,"message":{...}}
{"type":"message","id":"a1","parent_id":"u1","timestamp":...,"message":{...}}
```

Tree structure with `parent_id` links enables branching and forking.

### Tool Selection

```bash
# Allow only read and bash
pi --tools read,bash --print "inspect the repository"

# Disable all built-in tools
pi --no-tools --print "answer from the prompt only"

# Current runtime has no non-built-in tool surface, so this behaves the same
pi --no-builtin-tools --print "answer from the prompt only"
```

### WASM Extensions

Enable with `feat-extensions` feature:

```bash
cargo build --release --features feat-all,feat-extensions
```

Load custom tools via wasmtime runtime (see `pi-extension-system/` for details).

## Troubleshooting

### Common Issues

**"Max turns (200) reached"**
- The default runtime cap is 200 turns
- Increase it with `--max-turns <n>` or switch to interactive mode

**"Stream error: connection timeout"**
- Check network connectivity
- Verify API endpoint is accessible
- Try increasing timeout in settings

**"Invalid API key"**
- Verify key format matches provider requirements
- Check environment variables are set correctly
- Try OAuth login: `pi --login <provider>`

**Large binary size**
- Disable unused providers: `--no-default-features --features feat-openai`
- Avoid `feat-extensions` unless needed (wasmtime is heavy)

## Contributing

Contributions are welcome! Please:

1. Fork the repository
2. Create a feature branch
3. Run tests: `cargo test`
4. Format code: `cargo fmt`
5. Lint: `cargo clippy`
6. Submit a pull request

## License

MIT License - see workspace `Cargo.toml` for details.

## Acknowledgments

This is a Rust port of the TypeScript-based pi-coding-agent project, reimagined with Rust's performance, safety, and concurrency features.

## Links

- **Releases**: <https://github.com/charSLee013/PiAgentForge/releases>
- **Release process**: See `RELEASING.md`
- **CLI reference**: `cargo run -q -p pi -- --help`
- **Issues**: Report bugs and feature requests on GitHub
- **Discussions**: Join the community for questions and ideas
