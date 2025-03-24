# Address Smith

A Rust library for managing Ethereum wallet operations and transactions with built-in retry mechanisms and parallel execution capabilities.

## Features

- Parallel wallet operation execution with dependency management
- Automatic transaction retry with exponential backoff
- Comprehensive tracing for logging and terminal progress updates
- [INSECURE] plaintext wallet backup and management
- Custom cache for gas prices to avoid spamming the eth_gasPrice RPC method
- Detailed execution statistics and error impact analysis
- Progress tracking with estimated time remaining
- Support for complex operation trees with parent-child dependencies

## Architecture

The library is organized into several key components:

- `WalletManager`: Main entry point for wallet operations
- `TransactionManager`: Handles transaction building and sending
- `ExecutionManager`: Manages parallel execution of operations
- `ProgressManager`: Tracks and reports execution progress
- `CacheLayer`: Caches RPC responses to reduce API calls

## Configuration

The application is configured through environment variables. Copy `.env.example` to `.env` and adjust the values:

- `RPC_URL`: Desired network RPC URL (required)
- `PRIVATE_KEY`: Private key for the root wallet (required)
- `ADDRESS_COUNT`: Number of addresses to activate (required)
- `GAS_BUFFER_MULTIPLIER`: Multiplier for gas buffer (default: 2)
- `MAX_RETRIES`: Maximum number of retry attempts for failed transactions (default: 3)
- `RETRY_BASE_DELAY_MS`: Base delay between retry attempts in milliseconds (default: 1000)
- `RUST_LOG`: Log level (default: info)
- `SPLIT_LOOPS_COUNT`: Number of parallel operation loops (default: 2)
- `AMOUNT_PER_WALLET`: Amount of ETH to send to each new wallet (default: 1)

## Usage

1. Set up your environment variables in `.env`
2. Run the application:

```bash
cargo run --release
```

The application will:
1. Load configuration from environment variables
2. Connect to the specified Ethereum network
3. Execute wallet operations with automatic retry on failure
4. Generate detailed execution statistics and logs

## Error Handling

The library provides comprehensive error handling:
- Automatic retry for transient failures
- Detailed error impact analysis
- Transaction receipt verification
- Gas price caching to handle RPC rate limits
- Progress tracking with failure reporting

## Logging

Logs are written to both:
- Terminal: Real-time progress updates and statistics
- File: Detailed JSON-formatted logs in the `logs` directory

## Security Considerations

⚠️ **Warning**: This library includes plaintext wallet backup functionality which is inherently insecure. Use at your own risk and ensure proper security measures are in place.
