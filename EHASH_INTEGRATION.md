# Ehash Payment Method Support for CDK-Axum

This implementation adds support for the **ehash payment method** to the CDK-Axum web server. The ehash payment method allows users to mint tokens by solving hash challenges rather than making traditional Lightning Network payments.

## Overview

The ehash payment method is a novel approach to token minting that uses proof-of-work style hash challenges. Users provide a nonce, receive a hash challenge, and can mint tokens by providing a valid preimage that produces the target hash.

## Features Added

### 1. **Ehash Router Module** (`src/ehash_router.rs`)
- `POST /v1/mint/quote/ehash` - Request an ehash mint quote
- `GET /v1/mint/quote/ehash/{quote_id}` - Check the status of an ehash quote
- `POST /v1/mint/ehash` - Mint tokens using an ehash quote
- `POST /v1/ehash/submit/{quote_id}` - Submit hash solution for a quote

### 2. **Router Integration** (`src/lib.rs`)
- Added `create_ehash_router()` function
- Modified `create_mint_router()` to accept `include_ehash` parameter
- Integrated ehash routes into the main router when enabled

### 3. **Types and Conversions** (`crates/cdk/src/mint/issue/mod.rs`)
- Added `From<MintQuoteEhashRequest> for MintQuoteRequest` conversion
- Enhanced mint quote processing to handle ehash requests

### 4. **Dependencies**
- Added `cdk-ehash` dependency to support hash-based payment processing

## API Endpoints

### Create Ehash Mint Quote
```http
POST /v1/mint/quote/ehash
Content-Type: application/json

{
  "nonce": "user_provided_nonce",
  "amount": 100,
  "unit": "EHASH",
  "description": "Hash payment for tokens"
}
```

**Response:**
```json
{
  "quote": "quote_uuid",
  "challenge": "target_hash_to_solve",
  "amount": 100,
  "unit": "EHASH",
  "state": "UNPAID",
  "expiry": 1640995200,
  "nonce": "user_provided_nonce"
}
```

### Check Ehash Quote Status
```http
GET /v1/mint/quote/ehash/{quote_id}
```

### Mint Tokens with Ehash
```http
POST /v1/mint/ehash
Content-Type: application/json

{
  "quote": "quote_uuid",
  "outputs": [
    {
      "amount": 100,
      "id": "keyset_id",
      "B_": "blinded_message"
    }
  ]
}
```

### Submit Hash Solution
```http
POST /v1/ehash/submit/{quote_id}
Content-Type: application/json

{
  "preimage": "solution_that_produces_target_hash"
}
```

## Usage

### Basic Setup
```rust
use cdk_axum::create_mint_router;
use std::sync::Arc;

// Create mint instance (implementation specific)
let mint = Arc::new(create_mint_instance());

// Create router with ehash support enabled
let router = create_mint_router(
    mint,
    false, // include_bolt12
    true   // include_ehash
).await?;
```

### Advanced Usage with Custom Cache
```rust
use cdk_axum::{create_mint_router_with_custom_cache, cache::HttpCache};

let custom_cache = HttpCache::new();
let router = create_mint_router_with_custom_cache(
    mint,
    custom_cache,
    false, // include_bolt12  
    true   // include_ehash
).await?;
```

## Implementation Details

### Hash Challenge Flow
1. **Quote Creation**: User requests ehash quote with nonce
2. **Challenge Generation**: System generates target hash challenge
3. **Solution Submission**: User submits preimage that produces target hash
4. **Token Minting**: Valid solutions allow token minting

### Current Status
- ✅ Basic route structure implemented
- ✅ Type conversions and integrations
- ✅ Swagger/OpenAPI documentation
- ⚠️ Authentication placeholders (pending route path additions)
- 🔄 Integration with cdk-ehash backend (basic structure in place)

### Authentication
Currently, auth verification is temporarily disabled for ehash routes as the specific route paths need to be added to the core `RoutePath` enum. This will be enabled once the proper route definitions are added to the core library.

## Testing

Run the ehash integration tests:
```bash
cargo test -p cdk-axum ehash
```

## Future Enhancements

1. **Complete Integration**: Full integration with cdk-ehash payment processor
2. **Route Authentication**: Add ehash route paths to core RoutePath enum
3. **Mining Pool Integration**: Connect to external mining pools for challenge distribution
4. **Difficulty Adjustment**: Dynamic hash difficulty based on network conditions
5. **Batch Processing**: Support for multiple simultaneous hash challenges

## Configuration

The ehash payment method can be configured through mint settings to control:
- Minimum/maximum amounts
- Challenge difficulty
- Expiry times
- Fee structures

## Security Considerations

- Hash challenges should have appropriate difficulty to prevent spam
- Preimage validation must be cryptographically secure
- Rate limiting should be implemented for quote requests
- Proper error handling for invalid solutions

## Dependencies

This implementation relies on:
- `cdk-ehash`: Hash-based payment processing
- `cdk-common`: Common types and utilities
- `axum`: Web framework
- `uuid`: Unique identifiers
- `serde_json`: JSON serialization

## Contributing

When extending this implementation:
1. Follow existing patterns for payment method integration
2. Add comprehensive tests for new functionality
3. Update documentation for API changes
4. Consider backwards compatibility

---

This implementation provides the foundation for ehash payment method support in CDK-Axum, enabling innovative proof-of-work based token minting alongside traditional Lightning Network payments.
