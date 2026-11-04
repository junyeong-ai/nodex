//! `--baseline <REF>`: the ref one invocation's locks are judged against, in
//! place of `rules.immutable_baseline` ([`nodex_core::Config::set_invocation_baseline`]).
//!
//! Every command that resolves a baseline flattens [`BaselineArg`] — `check`,
//! `query issues` and each writing command — so a write and the
//! `check --content` gating it can be judged against the same commit. A
//! command that never resolves a baseline does not take the flag.

use clap::Args;

#[derive(Args, Clone, Debug, Default)]
pub struct BaselineArg {
    /// Judge the immutability locks against REF for this run, in place of
    /// `rules.immutable_baseline`, exactly as if `nodex.toml` named it there:
    /// `immutable_baseline_mode` still selects the commit from it. CI passes
    /// the commit its event is measured from — a merge group's base, the first
    /// parent of a pull request's merge, the commit a push replaced — so the
    /// verdict is a function of the commits judged rather than of where a
    /// fetched branch stood when the run read it
    #[arg(long, value_name = "REF")]
    pub baseline: Option<String>,
}

impl BaselineArg {
    /// `config` as this invocation judges it: with the ref `--baseline`
    /// names, where it names one. Applied wherever the command loads the
    /// config it judges with, the index's included.
    pub fn apply_to(&self, config: &mut nodex_core::Config) {
        if let Some(reference) = &self.baseline {
            config.set_invocation_baseline(reference.clone());
        }
    }
}
