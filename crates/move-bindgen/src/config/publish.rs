//! `[publish]` config block. Drives generation of deployable bytecode
//! per target network.
//!
//! Each entry under `networks` is either a bare string (name doubles as
//! `chain_id`) or a table with optional `chain_id` override and
//! `addresses` map for filling in named addresses that `Move.lock`
//! doesn't cover.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use move_core_types::account_address::AccountAddress;
use serde::Deserialize;

/// Validated `[publish]` block.
#[derive(Debug, Clone, Default)]
pub struct PublishConfig {
    pub networks: Vec<PublishNetwork>,
}

/// One target network for publish-bytes generation.
#[derive(Debug, Clone)]
pub struct PublishNetwork {
    /// Name used to refer to this network in the generated API
    /// (becomes a variant of the generated `Network` enum).
    pub name: String,
    /// `chain_id` passed to the Move build. Defaults to `name` when
    /// unset — the common case is `name == chain_id`.
    pub chain_id: Option<String>,
    /// Named-address overrides applied to the publish build, on top of
    /// `Move.lock` resolution. Used for networks not in the lock or to
    /// override individual entries.
    pub addresses: BTreeMap<String, AccountAddress>,
}

impl PublishNetwork {
    /// `chain_id` used at build time. Falls back to the network's name.
    pub fn effective_chain_id(&self) -> &str {
        self.chain_id.as_deref().unwrap_or(&self.name)
    }
}

/// Raw `[publish]` block as it appears in TOML.
#[derive(Debug, Deserialize, Default)]
pub(super) struct RawPublish {
    #[serde(default)]
    pub networks: Vec<RawNetwork>,
}

/// Raw network entry. `untagged` so both forms parse:
///   - bare string: `"testnet"`
///   - table: `{ name = "...", chain_id = "...", addresses = { ... } }`
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum RawNetwork {
    /// Bare string form. Name doubles as `chain_id`.
    Bare(String),
    /// Table form. Lets the user supply `chain_id` and/or `addresses`.
    Table {
        name: String,
        #[serde(default)]
        chain_id: Option<String>,
        #[serde(default)]
        addresses: BTreeMap<String, String>,
    },
}

impl PublishConfig {
    pub(super) fn from_raw(raw: RawPublish) -> Result<Self> {
        let mut networks = Vec::with_capacity(raw.networks.len());
        let mut seen = BTreeMap::new();
        for (i, raw_net) in raw.networks.into_iter().enumerate() {
            let net = PublishNetwork::from_raw(raw_net)
                .with_context(|| format!("[publish.networks][{i}]"))?;
            if !is_valid_network_name(&net.name) {
                bail!(
                    "[publish.networks][{i}].name = '{}' — must be a non-empty identifier (letters, digits, underscore, hyphen)",
                    net.name
                );
            }
            if let Some(prev_idx) = seen.insert(net.name.clone(), i) {
                bail!(
                    "[publish.networks][{i}] duplicates name '{}' (also at index {prev_idx})",
                    net.name
                );
            }
            networks.push(net);
        }
        Ok(Self { networks })
    }
}

impl PublishNetwork {
    fn from_raw(raw: RawNetwork) -> Result<Self> {
        match raw {
            RawNetwork::Bare(name) => Ok(Self {
                name,
                chain_id: None,
                addresses: BTreeMap::new(),
            }),
            RawNetwork::Table {
                name,
                chain_id,
                addresses,
            } => {
                let parsed = addresses
                    .into_iter()
                    .map(|(k, v)| {
                        let addr = AccountAddress::from_hex_literal(&v).with_context(|| {
                            format!("addresses.{k} = '{v}': not a valid hex address")
                        })?;
                        Ok::<_, anyhow::Error>((k, addr))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                Ok(Self {
                    name,
                    chain_id,
                    addresses: parsed,
                })
            }
        }
    }
}

/// Network names become Rust enum variants and a filename
/// (`bytecode/<name>.rs`). Keep them to a conservative subset.
fn is_valid_network_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<PublishConfig> {
        let raw: RawPublish = toml::from_str(toml_str).context("parsing test toml")?;
        PublishConfig::from_raw(raw)
    }

    #[test]
    fn empty_networks_parses() {
        let cfg = parse("networks = []").unwrap();
        assert!(cfg.networks.is_empty());
    }

    #[test]
    fn bare_string_form() {
        let cfg = parse(r#"networks = ["testnet", "mainnet"]"#).unwrap();
        assert_eq!(cfg.networks.len(), 2);
        assert_eq!(cfg.networks[0].name, "testnet");
        assert_eq!(cfg.networks[0].chain_id, None);
        assert_eq!(cfg.networks[0].effective_chain_id(), "testnet");
        assert!(cfg.networks[0].addresses.is_empty());
        assert_eq!(cfg.networks[1].name, "mainnet");
    }

    #[test]
    fn table_form_with_addresses() {
        let cfg = parse(
            r#"
            networks = [
                { name = "localnet", addresses = { iota = "0x2", std = "0x1" } },
            ]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.networks.len(), 1);
        let n = &cfg.networks[0];
        assert_eq!(n.name, "localnet");
        assert_eq!(n.chain_id, None);
        assert_eq!(n.effective_chain_id(), "localnet");
        assert_eq!(n.addresses.len(), 2);
        assert_eq!(
            n.addresses.get("iota"),
            Some(&AccountAddress::from_hex_literal("0x2").unwrap())
        );
        assert_eq!(
            n.addresses.get("std"),
            Some(&AccountAddress::from_hex_literal("0x1").unwrap())
        );
    }

    #[test]
    fn table_form_with_chain_id_override() {
        let cfg = parse(
            r#"
            networks = [
                { name = "ci", chain_id = "localnet" },
            ]
            "#,
        )
        .unwrap();
        let n = &cfg.networks[0];
        assert_eq!(n.name, "ci");
        assert_eq!(n.chain_id.as_deref(), Some("localnet"));
        assert_eq!(n.effective_chain_id(), "localnet");
    }

    #[test]
    fn mixed_bare_and_table() {
        let cfg = parse(
            r#"
            networks = [
                "testnet",
                "mainnet",
                { name = "localnet", addresses = { iota = "0x2" } },
            ]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.networks.len(), 3);
        assert_eq!(cfg.networks[0].name, "testnet");
        assert_eq!(cfg.networks[2].name, "localnet");
        assert_eq!(cfg.networks[2].addresses.len(), 1);
    }

    #[test]
    fn rejects_duplicate_names() {
        let err = parse(r#"networks = ["testnet", "testnet"]"#).unwrap_err();
        assert!(
            format!("{err:#}").contains("duplicates name 'testnet'"),
            "got: {err:#}"
        );
    }

    #[test]
    fn rejects_duplicate_via_table_and_bare() {
        let err = parse(
            r#"
            networks = ["testnet", { name = "testnet", addresses = { x = "0x1" } }]
            "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("duplicates name 'testnet'"));
    }

    #[test]
    fn rejects_invalid_name() {
        let err = parse(r#"networks = [""]"#).unwrap_err();
        assert!(format!("{err:#}").contains("must be a non-empty identifier"));

        let err = parse(r#"networks = ["has space"]"#).unwrap_err();
        assert!(
            format!("{err:#}").contains("must be a non-empty identifier"),
            "got: {err:#}"
        );
    }

    #[test]
    fn rejects_invalid_address() {
        let err = parse(
            r#"
            networks = [{ name = "localnet", addresses = { iota = "not-hex" } }]
            "#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("not a valid hex address"),
            "got: {err:#}"
        );
    }
}
