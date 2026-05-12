//! Synthetic-address assignment for `_`-valued `[addresses]` entries.
//! Walks every staged Move.toml, collects placeholders, and assigns
//! each a deterministic `0xff_…_NNNN` value reused across packages
//! that name the same address.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use move_core_types::account_address::AccountAddress;

use super::move_deps::read_addresses_block;
use crate::install_manifest::StagedPackage;

/// Walk every staged package's `[addresses]` block, find names whose
/// value is `"_"`, and assign each a deterministic synthetic
/// `0xff_…_NNNN` address. Same name across multiple packages gets the
/// same override (so cross-package refs stay consistent).
pub(super) fn build_address_overrides(
    staging_root: &Path,
    staged: &[StagedPackage],
) -> Result<BTreeMap<String, String>> {
    let mut overrides: BTreeMap<String, String> = BTreeMap::new();
    let mut next: u128 = 0xff00_0000_0000_0001;
    for pkg in staged {
        let toml_path = staging_root.join(&pkg.staged_path).join("Move.toml");
        let names = read_addresses_block(&toml_path)?;
        for (name, val) in names {
            if val != "_" || overrides.contains_key(&name) {
                continue;
            }
            overrides.insert(name, format_hex_address(synthetic_address(next)));
            next += 1;
        }
    }
    Ok(overrides)
}

fn synthetic_address(n: u128) -> AccountAddress {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&n.to_be_bytes());
    AccountAddress::new(bytes)
}

fn format_hex_address(addr: AccountAddress) -> String {
    addr.to_canonical_string(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_addresses_are_distinct() {
        let a = format_hex_address(synthetic_address(0xff00_0000_0000_0001));
        let b = format_hex_address(synthetic_address(0xff00_0000_0000_0002));
        assert_ne!(a, b);
        // Canonical hex is 64 chars + `0x`; the synthetic prefix lives in
        // the low 16 bytes of the address, so the marker shows up near
        // the end rather than at the start.
        assert!(a.ends_with("ff00000000000001"));
    }
}
