//! Ordering of immutable numeric Maven releases and conventional preview qualifiers.
use anyhow::{Result, ensure};
use std::cmp::Ordering;
fn number(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}
fn tokens(v: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut digit = None;
    for c in v.to_ascii_lowercase().chars() {
        if !c.is_ascii_alphanumeric() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            digit = None;
            continue;
        }
        let next = c.is_ascii_digit();
        if digit.is_some_and(|old| old != next) {
            result.push(std::mem::take(&mut current));
        }
        current.push(c);
        digit = Some(next);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}
fn qualifier(v: Option<&str>) -> (u8, &str) {
    match v.unwrap_or("") {
        "dev" | "eap" => (0, ""),
        "a" | "alpha" => (1, ""),
        "b" | "beta" => (2, ""),
        "m" | "milestone" => (3, ""),
        "cr" | "rc" => (4, ""),
        "snapshot" => (5, ""),
        "" | "ga" | "final" | "release" => (6, ""),
        "sp" => (7, ""),
        other => (8, other),
    }
}
pub fn compare(a: &str, b: &str) -> Result<Ordering> {
    let a = tokens(a);
    let b = tokens(b);
    let split = |v: &Vec<String>| {
        v.iter()
            .position(|s| !s.bytes().all(|c| c.is_ascii_digit()))
            .unwrap_or(v.len())
    };
    let ai = split(&a);
    let bi = split(&b);
    ensure!(
        ai > 0 && bi > 0,
        "numeric pinned version required for Android mediation"
    );
    for i in 0..ai.max(bi) {
        let x = a
            .get(i)
            .filter(|_| i < ai)
            .map(String::as_str)
            .unwrap_or("0");
        let y = b
            .get(i)
            .filter(|_| i < bi)
            .map(String::as_str)
            .unwrap_or("0");
        let order = number(x, y);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    let aq = qualifier(a.get(ai).map(String::as_str));
    let bq = qualifier(b.get(bi).map(String::as_str));
    ensure!(
        aq.0 != 8 || bq.0 != 8 || aq.1 == bq.1,
        "incomparable version flavours; declare a project constraint"
    );
    if aq.0 != bq.0 {
        return Ok(aq.0.cmp(&bq.0));
    }
    let an = &a[ai.saturating_add(1).min(a.len())..];
    let bn = &b[bi.saturating_add(1).min(b.len())..];
    for i in 0..an.len().max(bn.len()) {
        let x = an.get(i).map(String::as_str).unwrap_or("0");
        let y = bn.get(i).map(String::as_str).unwrap_or("0");
        ensure!(
            x.bytes().all(|c| c.is_ascii_digit()) && y.bytes().all(|c| c.is_ascii_digit()),
            "unsupported version qualifier tail"
        );
        let order = number(x, y);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(Ordering::Equal)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_components_and_zero_padding_are_not_lexical() {
        assert_eq!(compare("1.10.0", "1.9.99").unwrap(), Ordering::Greater);
        assert_eq!(compare("1.0", "1.0.0").unwrap(), Ordering::Equal);
        assert_eq!(compare("1.0002", "1.2").unwrap(), Ordering::Equal);
    }
    #[test]
    fn stable_release_is_newer_than_preview_and_preview_numbers_are_numeric() {
        assert_eq!(
            compare("1.4.0", "1.4.0-alpha18").unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            compare("1.4.0-alpha18", "1.4.0-alpha9").unwrap(),
            Ordering::Greater
        );
        assert_eq!(
            compare("2.0.0-RC2", "2.0.0-Beta5").unwrap(),
            Ordering::Greater
        );
    }
    #[test]
    fn platform_flavour_conflicts_require_an_explicit_pin() {
        assert!(compare("33.0-jre", "33.0-android").is_err());
        assert_eq!(
            compare("33.1-android", "32.0-android").unwrap(),
            Ordering::Greater
        );
    }
}
