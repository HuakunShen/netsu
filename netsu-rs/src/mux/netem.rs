//! Validation for `tc netem` network-condition profiles. This module does not
//! apply anything (that happens in the Docker entrypoint via `tc`); it only
//! validates the values, rejecting anything that isn't a plain
//! number-with-unit so a profile string can never smuggle shell metacharacters
//! into the `tc` command.

use std::collections::BTreeMap;

use anyhow::{bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetemProfile {
    pub rate: String,
    pub delay: String,
    pub jitter: String,
    pub loss: String,
    pub reorder: String,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetemProfileSet {
    pub profiles: BTreeMap<String, NetemProfile>,
}

/// Only digits, an optional decimal point, and a known unit suffix are allowed
/// — no spaces, semicolons, backticks, `$`, etc.
fn valid_scalar(value: &str, units: &[&str]) -> bool {
    let unit = units.iter().find(|u| value.ends_with(**u));
    let Some(unit) = unit else { return false };
    let num = &value[..value.len() - unit.len()];
    !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit() || c == '.')
        && num.matches('.').count() <= 1
}

impl NetemProfile {
    /// Reject anything that isn't a plain number-with-unit.
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            valid_scalar(&self.rate, &["mbit", "kbit", "gbit"]),
            "bad rate: {}",
            self.rate
        );
        ensure!(
            valid_scalar(&self.delay, &["ms", "us", "s"]),
            "bad delay: {}",
            self.delay
        );
        ensure!(
            valid_scalar(&self.jitter, &["ms", "us", "s"]),
            "bad jitter: {}",
            self.jitter
        );
        ensure!(valid_scalar(&self.loss, &["%"]), "bad loss: {}", self.loss);
        ensure!(
            valid_scalar(&self.reorder, &["%"]),
            "bad reorder: {}",
            self.reorder
        );
        Ok(())
    }
}

impl NetemProfileSet {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.profiles.is_empty() {
            bail!("netem profile set is empty");
        }
        for (name, p) in &self.profiles {
            p.validate()
                .map_err(|e| anyhow::anyhow!("profile {name}: {e}"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(rate: &str, loss: &str) -> NetemProfile {
        NetemProfile {
            rate: rate.into(),
            delay: "50ms".into(),
            jitter: "5ms".into(),
            loss: loss.into(),
            reorder: "0%".into(),
            limit: 500,
        }
    }

    #[test]
    fn accepts_plain_values() {
        profile("100mbit", "0.1%").validate().unwrap();
        profile("1gbit", "5%").validate().unwrap();
    }

    #[test]
    fn rejects_shell_metacharacters_and_bad_units() {
        assert!(profile("100mbit; rm -rf /", "0%").validate().is_err());
        assert!(profile("100mbit", "0.1%$(whoami)").validate().is_err());
        assert!(profile("100", "0%").validate().is_err()); // no unit
        assert!(profile("100mbit", "0.1").validate().is_err()); // loss needs %
        assert!(profile("100mbit`id`", "0%").validate().is_err());
    }
}
