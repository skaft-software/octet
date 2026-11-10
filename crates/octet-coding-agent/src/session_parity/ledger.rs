//! The expected-divergence ledger for the session parity harness.
//!
//! This file is the only permitted form of "known gap". A divergence that is
//! not listed here fails the harness, and a listed entry that no longer matches
//! anything also fails, so fixing a gap means deleting its line.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

use super::canonical::Mismatch;

#[derive(Deserialize)]
struct File {
    gaps: Vec<Gap>,
}

#[derive(Deserialize)]
pub(super) struct Gap {
    /// Scenario name, or `*` when the divergence is identical everywhere.
    scenario: String,
    /// Step index, or `*` when it repeats at every step of the scenario.
    step: Value,
    check: String,
    /// `entries.*.payload.toolName` style; `*` matches exactly one segment.
    fingerprint: String,
    pub(super) reason: String,
}

impl Gap {
    fn step_matches(&self, step: usize) -> bool {
        match &self.step {
            Value::String(text) => text.as_str() == "*",
            Value::Number(number) => number.as_u64() == Some(step as u64),
            _ => false,
        }
    }
}

/// A fingerprint pattern where `*` matches one dot-separated segment.
fn fingerprint_matches(pattern: &str, fingerprint: &str) -> bool {
    let mut pattern = pattern.split('.');
    let mut fingerprint = fingerprint.split('.');
    loop {
        match (pattern.next(), fingerprint.next()) {
            (None, None) => return true,
            (Some("*"), Some(_)) => continue,
            (Some(expected), Some(actual)) if expected == actual => continue,
            _ => return false,
        }
    }
}

pub(super) struct Ledger {
    gaps: Vec<Gap>,
    matched: Vec<bool>,
}

impl Ledger {
    pub(super) fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path).map_err(|error| {
            anyhow::anyhow!(
                "cannot read the session parity ledger {}: {error}",
                path.display()
            )
        })?;
        let file: File = serde_json::from_slice(&bytes)?;
        let matched = vec![false; file.gaps.len()];
        Ok(Self {
            gaps: file.gaps,
            matched,
        })
    }

    /// Record `mismatch` against the ledger. Returns false when it is unknown.
    pub(super) fn accept(&mut self, scenario: &str, step: usize, mismatch: &Mismatch) -> bool {
        let mut accepted = false;
        for (index, gap) in self.gaps.iter().enumerate() {
            if (gap.scenario == "*" || gap.scenario == scenario)
                && gap.step_matches(step)
                && gap.check == mismatch.check
                && fingerprint_matches(&gap.fingerprint, &mismatch.fingerprint)
            {
                self.matched[index] = true;
                accepted = true;
            }
        }
        accepted
    }

    pub(super) fn len(&self) -> usize {
        self.gaps.len()
    }

    /// Entries that the executed scenarios could have exercised but never
    /// matched: either a fixed gap or a stale expectation.
    pub(super) fn stale(&self, executed: &HashSet<String>) -> Vec<String> {
        self.gaps
            .iter()
            .zip(&self.matched)
            .filter(|(_, matched)| !**matched)
            .filter(|(gap, _)| gap.scenario == "*" || executed.contains(&gap.scenario))
            .map(|(gap, _)| {
                format!(
                    "{} step {} {} {} ({})",
                    gap.scenario, gap.step, gap.check, gap.fingerprint, gap.reason
                )
            })
            .collect()
    }
}
