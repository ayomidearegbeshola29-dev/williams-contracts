//! Storage schema versioning for persisted `Will` entries (issue #446).
//!
//! Soroban encodes a `#[contracttype]` struct as an XDR map keyed by field
//! name, and decoding is strict: a stored entry that is missing a field the
//! current `Will` declares (or carries one it no longer declares) fails to
//! convert. Adding a field to `Will` would therefore make every will written
//! by an earlier contract version unreadable.
//!
//! This module holds the two pieces that keep old entries loadable:
//!
//! 1. [`decode_will`] — every read of a `DataKey::Will` entry goes through
//!    here. It tries the current layout first and then each known legacy
//!    layout, lifting a legacy entry into the current `Will` shape while
//!    keeping its original `schema_version`.
//! 2. [`upgrade`] — applies the per-version migration steps in order until
//!    the will reaches [`CURRENT_SCHEMA_VERSION`]. Called by
//!    `WillContract::migrate_will`, which persists the result.
//!
//! See the "Storage schema versioning" section of CONTRIBUTING.md for the
//! step-by-step process of introducing a new version.
//!
//! ## Schema v2 field ordering (issue #117)
//!
//! Soroban XDR serializes struct fields in declaration order, so the order of
//! `Will`'s fields directly affects the encoded byte layout. In v2 the fields
//! are grouped by kind to keep fixed-size scalars and addresses adjacent:
//!
//! 1. Identity and token metadata: `id`, `owner`, `token`, `is_native`,
//!    `balance`, `balances`.
//! 2. Beneficiaries: `beneficiaries`, `hashed_beneficiaries`.
//! 3. Timing: `checkin_period_days`, `grace_period_days`, `last_checkin`,
//!    `trigger_time`, `confirmation_deadline`.
//! 4. Status and guardians: `status`, `guardians`, `guardian_vote_weight`,
//!    `guardian_votes`, `guardian_cancel_vote_weight`, `guardian_cancel_votes`,
//!    `guardian_threshold`, `guardian_list_updated_at`.
//! 5. Versioning and payout config: `schema_version`, `keeper_bounty_bps`,
//!    `keeper_bounty_token`, `delegate`.
//!
//! Because XDR maps are keyed by field name, this reordering is wire-compatible
//! with v1 entries: `decode_will` still lifts them via [`legacy::WillV1`], and
//! the v1 → v2 step only stamps the version. The legacy struct below keeps its
//! original declaration order verbatim so old entries continue to decode.

use soroban_sdk::{Env, TryFromVal, Val};

use crate::errors::WillError;
use crate::types::Will;

/// Schema version written into every newly created or migrated will.
///
/// Bump this whenever the persisted `Will` layout changes, and add both a
/// legacy decode arm in [`decode_will`] and a migration step in [`upgrade`].
///
/// Keep in sync with [`crate::storage::CURRENT_SCHEMA_VERSION`]; the two are
/// asserted equal at compile time in `migrate_will_test.rs`.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

/// `true` when this build is a pre-release (release candidate) of the
/// contract, `false` for a final build. See the module docs for how this
/// combines with `CONTRACT_VERSION` to identify an RC build.
pub const CONTRACT_PRERELEASE: bool = false;

/// Pre-release candidate number for this build (`1` for `rc.1`), or `0` when
/// [`CONTRACT_PRERELEASE`] is `false`.
pub const CONTRACT_PRERELEASE_NUM: u32 = 0;

/// Packs a SemVer triple into the `u32` encoding used by `CONTRACT_VERSION`.
///
/// Each component must be below 1_000; larger values would overlap adjacent
/// fields. This is the single source of truth for the encoding so the
/// contract constant and its tests cannot drift apart.
pub const fn encode_version(major: u32, minor: u32, patch: u32) -> u32 {
    major * 1_000_000 + minor * 1_000 + patch
}

/// Splits a packed version back into `(major, minor, patch)`.
pub const fn decode_version(version: u32) -> (u32, u32, u32) {
    (version / 1_000_000, (version / 1_000) % 1_000, version % 1_000)
}

/// Legacy `Will` layouts, frozen at the version their successor replaced.
///
/// A legacy struct is the **only** way an entry written by an older contract
/// version can still be decoded: `Will::try_from_val` fails outright when the
/// stored XDR map is missing a field the current layout declares. Each type
/// therefore copies its era's `Will` verbatim — field names and order
/// included — and converts into the current shape with safe defaults for
/// whatever the newer version added.
pub mod legacy {
    use soroban_sdk::{contracttype, Address, Map, Vec};

    use crate::types::{Beneficiary, Guardian, HashedBeneficiary, Will, WillStatus};

    /// The `Will` layout as of schema version 1 — the layout `schema_version`
    /// itself shipped in (#446), with `keeper_bounty_bps` and `delegate`
    /// already present.
    ///
    /// Superseded by v2, which added `Will::keeper_bounty_token` (#109) and
    /// regrouped the field declaration order for byte compactness (#117).
    #[contracttype]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct WillV1 {
        pub id: u64,
        pub owner: Address,
        pub balances: Map<Address, i128>,
        pub token: Address,
        pub is_native: bool,
        pub balance: i128,
        pub beneficiaries: Vec<Beneficiary>,
        pub hashed_beneficiaries: Vec<HashedBeneficiary>,
        pub checkin_period_days: u64,
        pub grace_period_days: u64,
        pub last_checkin: u64,
        pub trigger_time: Option<u64>,
        pub confirmation_deadline: Option<u64>,
        pub status: WillStatus,
        pub guardians: Vec<Guardian>,
        pub guardian_vote_weight: u32,
        pub guardian_votes: u32,
        pub guardian_cancel_vote_weight: u32,
        pub guardian_cancel_votes: u32,
        pub guardian_threshold: u32,
        pub guardian_list_updated_at: u64,
        pub schema_version: u32,
        pub keeper_bounty_bps: u32,
        pub delegate: Option<Address>,
    }

    impl WillV1 {
        /// Lifts a v1 entry into the current `Will` shape. The field v2 added
        /// defaults to `None` — the proportional keeper-bounty split (#109) —
        /// and `schema_version` stays at 1 so [`super::upgrade`] still runs
        /// the v1 → v2 step.
        pub fn into_current(self) -> Will {
            Will {
                id: self.id,
                owner: self.owner,
                token: self.token,
                is_native: self.is_native,
                balance: self.balance,
                balances: self.balances,
                beneficiaries: self.beneficiaries,
                hashed_beneficiaries: self.hashed_beneficiaries,
                checkin_period_days: self.checkin_period_days,
                grace_period_days: self.grace_period_days,
                last_checkin: self.last_checkin,
                trigger_time: self.trigger_time,
                confirmation_deadline: self.confirmation_deadline,
                status: self.status,
                guardians: self.guardians,
                guardian_vote_weight: self.guardian_vote_weight,
                guardian_votes: self.guardian_votes,
                guardian_cancel_vote_weight: self.guardian_cancel_vote_weight,
                guardian_cancel_votes: self.guardian_cancel_votes,
                guardian_threshold: self.guardian_threshold,
                guardian_list_updated_at: self.guardian_list_updated_at,
                schema_version: self.schema_version,
                keeper_bounty_bps: self.keeper_bounty_bps,
                keeper_bounty_token: None,
                delegate: self.delegate,
                trigger_cooldown_until: 0,
            }
        }
    }
}

/// Decodes a raw persisted will, accepting the current layout and every known
/// legacy layout.
///
/// A legacy entry is converted into the current `Will` shape with defaults
/// for fields it did not have; its `schema_version` is left at the version it
/// was written with so [`upgrade`] can run the remaining steps.
///
/// Returns [`WillError::UnsupportedSchemaVersion`] if no known layout matches.
pub fn decode_will(env: &Env, raw: &Val) -> Result<Will, WillError> {
    if let Ok(will) = Will::try_from_val(env, raw) {
        return Ok(will);
    }

    // Legacy layouts are tried here, newest first.
    if let Ok(old) = legacy::WillV1::try_from_val(env, raw) {
        return Ok(old.into_current());
    }

    Err(WillError::UnsupportedSchemaVersion)
}

/// Runs every migration step between `will.schema_version` and
/// [`CURRENT_SCHEMA_VERSION`], in order. A will already at (or beyond) the
/// current version is returned unchanged.
pub fn upgrade(env: &Env, mut will: Will) -> Will {
    while will.schema_version < CURRENT_SCHEMA_VERSION {
        will = match will.schema_version {
            0 => migrate_v0_to_v1(env, will),
            1 => migrate_v1_to_v2(env, will),
            // Every version below CURRENT_SCHEMA_VERSION must have a step.
            _ => soroban_sdk::panic_with_error!(env, WillError::UnsupportedSchemaVersion),
        };
    }
    will
}

/// v0 → v1: v0 wills predate the `schema_version` field being populated; the
/// layout is otherwise identical, so only the version number changes.
fn migrate_v0_to_v1(_env: &Env, mut will: Will) -> Will {
    will.schema_version = 1;
    will
}

/// v1 → v2: v2 added `Will::keeper_bounty_token` (#109) and regrouped the
/// `Will` field declaration order for byte compactness (#117). `decode_will`
/// has already lifted the entry into the current shape with the new field
/// defaulted to `None` (proportional bounty split), so the step only stamps
/// the new version.
fn migrate_v1_to_v2(_env: &Env, mut will: Will) -> Will {
    will.schema_version = 2;
    will
}

#[cfg(test)]
mod version_tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let packed = encode_version(1, 3, 0);
        assert_eq!(packed, 1_003_000);
        assert_eq!(decode_version(packed), (1, 3, 0));
    }

    #[test]
    fn decode_handles_max_components() {
        let packed = encode_version(999, 999, 999);
        assert_eq!(decode_version(packed), (999, 999, 999));
    }

    #[test]
    fn prerelease_flag_is_consistent() {
        // A pre-release build must carry a candidate number; a final build
        // must not. This is what client bindings rely on to detect RCs.
        if CONTRACT_PRERELEASE {
            assert!(CONTRACT_PRERELEASE_NUM >= 1);
        } else {
            assert_eq!(CONTRACT_PRERELEASE_NUM, 0);
        }
    }
}
