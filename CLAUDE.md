# pi-to-rust — Pi Agent Rust Port

## Build

```bash
# Default (OpenAI provider only)
cargo build --release --package pi

# With Anthropic provider
cargo build --release --package pi --features feat-anthropic

# All providers
cargo build --release --package pi --features feat-all
```

## Run

```bash
pi --print "your prompt"
pi --provider anthropic --model claude-sonnet-4-20250514 --print "hello"
pi --provider openai --model deepseek-v4-pro --base-url <url> --print "hello"
```

## Key Features

- **feat-anthropic**: Adds Anthropic Messages API provider. Reads `ANTHROPIC_BASE_URL` env var (defaults to `https://api.anthropic.com/v1/messages`). Wired through pi-cli feature gate matching feat-bedrock pattern.
- **feat-bedrock**: AWS Bedrock provider.
- **max_turns**: 200 in print mode (hardcoded in `pi-cli/src/lib.rs:353`).
