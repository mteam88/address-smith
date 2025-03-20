# Active Address

A Rust library for managing Ethereum wallet operations and transactions with built-in retry mechanisms and parallel execution capabilities.

## Features

- Sequential and parallel wallet operation execution
- Automatic transaction retry with exponential backoff
- Comprehensive transaction cost tracking and statistics
- Secure wallet backup and management
- Configurable gas pricing and transaction parameters

## Configuration

The application is configured through environment variables. Copy `.env.example` to `.env` and adjust the values:

- `RPC_URL`: Ethereum network RPC URL (required)
- `PRIVATE_KEY`: Private key for the root wallet (required)
- `ADDRESS_COUNT`: Number of addresses to activate (required)
- `GAS_BUFFER_MULTIPLIER`: Multiplier for gas buffer (default: 2)
- `MAX_RETRIES`: Maximum number of retry attempts for failed transactions (default: 3)
- `RETRY_BASE_DELAY_MS`: Base delay between retry attempts in milliseconds (default: 1000)
- `RUST_LOG`: Log level (default: info)

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

The application includes robust error handling with:
- Automatic transaction retry with exponential backoff
- Detailed error logging
- Transaction receipt verification
- Gas price monitoring and adjustment

## Logging

Logs are written to both console and file:
- Console output uses the specified `RUST_LOG` level
- File logs are stored in `wallet_manager_[id].log`
- Each operation and transaction is logged with timestamps and details