//! Move chain flavour — chooses which build chain, runtime crate,
//! and SDK family the generated code targets.

/// Move chain flavour — chooses which build chain, runtime crate,
/// and SDK family the generated code targets.
///
/// Flavour is per-project. Two flavours don't mix in one workspace
/// (different SDK type identities). Default is `Iota`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
#[clap(rename_all = "lowercase")]
pub enum Flavour {
    #[default]
    Iota,
    Sui,
}

impl Flavour {
    pub fn as_str(&self) -> &'static str {
        match self {
            Flavour::Iota => "iota",
            Flavour::Sui => "sui",
        }
    }
}
