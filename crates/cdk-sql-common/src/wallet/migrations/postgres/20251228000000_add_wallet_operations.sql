-- Migration to add wallet operations table and proof operation tracking

-- Create wallet_operations table
CREATE TABLE IF NOT EXISTS wallet_operations (
    id TEXT PRIMARY KEY,
    kind TEXT CHECK (kind IN ('send', 'receive', 'swap', 'mint', 'melt')) NOT NULL,
    state TEXT CHECK (state IN ('init', 'prepared', 'executing', 'pending', 'finalized', 'rolled_back')) NOT NULL,
    amount BIGINT NOT NULL,
    mint_url TEXT NOT NULL,
    unit TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    data TEXT NOT NULL
);

-- Create indexes for efficient queries
CREATE INDEX IF NOT EXISTS wallet_operations_state_index ON wallet_operations(state);
CREATE INDEX IF NOT EXISTS wallet_operations_mint_url_index ON wallet_operations(mint_url);
CREATE INDEX IF NOT EXISTS wallet_operations_kind_index ON wallet_operations(kind);
CREATE INDEX IF NOT EXISTS wallet_operations_created_at_index ON wallet_operations(created_at);

-- Add operation tracking columns to proof table
ALTER TABLE proof ADD COLUMN IF NOT EXISTS used_by_operation TEXT;
ALTER TABLE proof ADD COLUMN IF NOT EXISTS created_by_operation TEXT;

-- Create index for efficient operation-based proof queries
CREATE INDEX IF NOT EXISTS proof_used_by_operation_index ON proof(used_by_operation);
CREATE INDEX IF NOT EXISTS proof_created_by_operation_index ON proof(created_by_operation);
