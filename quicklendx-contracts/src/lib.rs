#![no_std]

extern crate alloc;

// --- Leaf modules (no crate:: dependencies beyond what's listed) ----------

/// Diagnostics macros (`assert_view_only!`, `qlx_log!`).
pub mod diagnostics;
/// Error code → Symbol mapping.
pub mod errors;
/// Leaf: governance proposal guard.
pub mod governance;
/// Leaf: idempotency key helpers.
pub mod idempotency;
/// Invoice amount precision and overflow validation (Issue #2432).
pub mod invoice_amount;
/// Leaf: observability hooks for events and audit.
pub mod observability;
/// Pagination helpers.
pub mod pagination;
/// Leaf: regulatory gate checks.
pub mod regulatory;

// --- Core data types ------------------------------------------------------

/// Core protocol data types (`Invoice`, `Bid`, enums, etc.).
pub mod types;

// --- Infrastructure modules ------------------------------------------------

/// Audit trail logging and chain verification.
pub mod audit;
/// Backup schema v1 (internal).
pub mod backup_v1;
/// Platform fee calculation and fee record storage.
pub mod fees;
/// Per-business active-invoice counter and investment limits.
pub mod investment;
/// Profit-distribution accounting.
pub mod profits;

// --- Storage & indexing ----------------------------------------------------

/// Persistent storage helpers, TTL management, and cross-module storage.
pub mod storage;

// --- Verification & KYC ---------------------------------------------------

/// KYC verification storage and business/investor verification logic.
pub mod verification;

// --- Bid lifecycle ---------------------------------------------------------

/// Bid storage, ranking, TTL, expiry cleanup, and rate limits.
pub mod bid;

// --- Invoice lifecycle -----------------------------------------------------

/// Invoice creation, state transitions, and default handling.
pub mod defaults;
/// Invoice creation, state machine, and metadata helpers.
pub mod invoice;

// --- Financial operations --------------------------------------------------

/// Escrow management and token-transfer-based payment flow.
pub mod payments;
/// Protocol-level resource limits, rate limiting, and input-size guards.
pub mod protocol_limits;

// --- Admin & configuration -------------------------------------------------

/// Admin storage and two-step handover.
pub mod admin;
/// Protocol initialisation, migration, and upgrade guards.
pub mod init;

// --- Backup ----------------------------------------------------------------

/// Backup creation, restore, and retention policy.
pub mod backup;

// --- Events ----------------------------------------------------------------

/// Event emission helpers for off-chain indexers.
pub mod events;

// --- Notifications ---------------------------------------------------------

/// Notification delivery and preference management.
pub mod notifications;

// --- Settlement ------------------------------------------------------------

/// Settlement lifecycle and fund distribution.
pub mod settlement;

// --- Invariants ------------------------------------------------------------

/// Cross-module invariant self-check (heartbeat).
pub mod invariants;

// --- Contract entry points -------------------------------------------------

/// Soroban contract entry points (`#[contract]` / `#[contractimpl]`).
pub mod contract;

// --- Test modules ----------------------------------------------------------

#[cfg(test)]
mod test_bid_resource_limits;
#[cfg(test)]
mod test_invoice_amount_precision;
