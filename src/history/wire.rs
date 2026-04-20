use serde::Deserialize;
use serde::Deserializer;
use std::collections::BTreeMap;

#[derive(Deserialize)]
pub struct ChartEnvelope {
    pub(crate) chart: Option<ChartNode>,
}

#[derive(Deserialize)]
pub struct ChartNode {
    pub(crate) result: Option<Vec<ChartResult>>,
    pub(crate) error: Option<ChartError>,
}

#[derive(Deserialize)]
pub struct ChartError {
    pub(crate) code: String,
    pub(crate) description: String,
}

#[derive(Deserialize)]
pub struct ChartResult {
    #[serde(default)]
    pub(crate) meta: Option<MetaNode>,
    #[serde(default)]
    pub(crate) timestamp: Option<Vec<i64>>,
    pub(crate) indicators: Indicators,
    #[serde(default)]
    pub(crate) events: Option<Events>,
}

#[derive(Deserialize, Clone)]
pub struct MetaNode {
    #[serde(default)]
    pub(crate) timezone: Option<String>,
    #[serde(default)]
    pub(crate) gmtoffset: Option<i64>,
    #[serde(default)]
    pub(crate) currency: Option<String>,
}

#[derive(Deserialize)]
pub struct Indicators {
    #[serde(default)]
    pub(crate) quote: Vec<QuoteBlock>,
    #[serde(default)]
    pub(crate) adjclose: Vec<AdjCloseBlock>,
}

#[derive(Deserialize, Clone)]
pub struct QuoteBlock {
    #[serde(default)]
    pub(crate) open: Vec<Option<f64>>,
    #[serde(default)]
    pub(crate) high: Vec<Option<f64>>,
    #[serde(default)]
    pub(crate) low: Vec<Option<f64>>,
    #[serde(default)]
    pub(crate) close: Vec<Option<f64>>,
    #[serde(default)]
    pub(crate) volume: Vec<Option<u64>>,
}

#[derive(Deserialize, Clone)]
pub struct AdjCloseBlock {
    #[serde(default)]
    pub(crate) adjclose: Vec<Option<f64>>,
}

#[derive(Deserialize, Default, Clone)]
pub struct Events {
    #[serde(default)]
    pub(crate) dividends: Option<BTreeMap<String, DividendEvent>>,
    #[serde(default)]
    pub(crate) splits: Option<BTreeMap<String, SplitEvent>>,
    #[serde(default, rename = "capitalGains")]
    pub(crate) capital_gains: Option<BTreeMap<String, CapitalGainEvent>>,
}

#[derive(Deserialize, Clone)]
pub struct DividendEvent {
    pub(crate) amount: Option<f64>,
    pub(crate) date: Option<i64>,
}

#[derive(Clone)]
pub struct SplitEvent {
    pub(crate) numerator: Option<u64>,
    pub(crate) denominator: Option<u64>,
    pub(crate) split_ratio: Option<String>,
    pub(crate) date: Option<i64>,
}

#[derive(Deserialize)]
struct SplitEventRaw {
    #[serde(default)]
    numerator: Option<serde_json::Value>,
    #[serde(default)]
    denominator: Option<serde_json::Value>,
    #[serde(rename = "splitRatio", default)]
    split_ratio: Option<String>,
    #[serde(default)]
    date: Option<i64>,
}

impl<'de> Deserialize<'de> for SplitEvent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = SplitEventRaw::deserialize(d)?;
        let (numerator, denominator) =
            normalize_split_ratio(raw.numerator.as_ref(), raw.denominator.as_ref())
                .map_err(D::Error::custom)?;
        Ok(SplitEvent {
            numerator,
            denominator,
            split_ratio: raw.split_ratio,
            date: raw.date,
        })
    }
}

#[derive(Deserialize, Clone)]
pub struct CapitalGainEvent {
    pub(crate) amount: Option<f64>,
    pub(crate) date: Option<i64>,
}

/// Precision scale applied when either side of a split ratio arrives as
/// a non-integer float. Ratio preserved to 6 decimal places — matches
/// observed Yahoo precision for preferred-share adjustments (e.g.
/// AXIA-P's 1.262838). Scaled values still fit u32 downstream for any
/// realistic split.
const FRACTIONAL_SPLIT_SCALE: u64 = 1_000_000;

#[derive(Debug)]
enum SplitField {
    Absent,
    Integer(u64),
    Fractional(f64),
}

/// Classify one side of a split ratio from the raw JSON value. Accepts
/// unsigned ints, integer-like floats, numeric strings, and fractional
/// finite non-negative floats. Returns `Absent` for null/missing.
fn classify_split_field(v: Option<&serde_json::Value>) -> Result<SplitField, String> {
    use serde_json::Value;
    let v = match v {
        None | Some(Value::Null) => return Ok(SplitField::Absent),
        Some(x) => x,
    };
    match v {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Ok(SplitField::Integer(u))
            } else if let Some(f) = n.as_f64() {
                if !f.is_finite() || f < 0.0 {
                    return Err(format!("non-finite or negative split field: {f}"));
                }
                let r = f.round();
                if (f - r).abs() < 1e-9 {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss
                    )]
                    Ok(SplitField::Integer(r as u64))
                } else {
                    Ok(SplitField::Fractional(f))
                }
            } else {
                Err("unsupported number type for split field".to_string())
            }
        }
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Ok(SplitField::Absent);
            }
            if let Ok(u) = s.parse::<u64>() {
                return Ok(SplitField::Integer(u));
            }
            if let Ok(f) = s.parse::<f64>() {
                if !f.is_finite() || f < 0.0 {
                    return Err(format!("non-finite or negative split field: '{s}'"));
                }
                return Ok(SplitField::Fractional(f));
            }
            Err(format!("invalid numeric string '{s}' for split field"))
        }
        other => Err(format!("unexpected JSON type for split field: {other}")),
    }
}

/// Normalize numerator/denominator jointly. If both sides are integer-
/// like (or absent), pass through unchanged. If either is fractional,
/// scale *both* by `FRACTIONAL_SPLIT_SCALE` so the rational representation
/// stays consistent across the pair. Integer `2` alongside fractional
/// `0.5` becomes `(2_000_000, 500_000)` — still a 4:1 ratio.
fn normalize_split_ratio(
    num: Option<&serde_json::Value>,
    den: Option<&serde_json::Value>,
) -> Result<(Option<u64>, Option<u64>), String> {
    let n = classify_split_field(num)?;
    let d = classify_split_field(den)?;

    let needs_scaling =
        matches!(n, SplitField::Fractional(_)) || matches!(d, SplitField::Fractional(_));

    if !needs_scaling {
        return Ok((integer_or_none(n), integer_or_none(d)));
    }

    Ok((scale_side(n), scale_side(d)))
}

fn integer_or_none(f: SplitField) -> Option<u64> {
    match f {
        SplitField::Absent => None,
        SplitField::Integer(u) => Some(u),
        SplitField::Fractional(_) => unreachable!("caller guarantees no fractional side"),
    }
}

fn scale_side(f: SplitField) -> Option<u64> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    match f {
        SplitField::Absent => None,
        SplitField::Integer(u) => u.checked_mul(FRACTIONAL_SPLIT_SCALE),
        SplitField::Fractional(v) => Some((v * FRACTIONAL_SPLIT_SCALE as f64).round() as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_pair_passes_through_unchanged() {
        let (n, d) = normalize_split_ratio(Some(&json!(2)), Some(&json!(1))).unwrap();
        assert_eq!((n, d), (Some(2), Some(1)));
    }

    #[test]
    fn integer_like_float_treated_as_integer() {
        let (n, d) = normalize_split_ratio(Some(&json!(4.0)), Some(&json!(1.0))).unwrap();
        assert_eq!((n, d), (Some(4), Some(1)));
    }

    #[test]
    fn fractional_numerator_scales_both_sides() {
        // AXIA-P real-world payload: 1.262838 / 1
        let (n, d) = normalize_split_ratio(Some(&json!(1.262838)), Some(&json!(1))).unwrap();
        assert_eq!((n, d), (Some(1_262_838), Some(1_000_000)));
        // Ratio still resolves to 1.262838 downstream.
        let ratio = n.unwrap() as f64 / d.unwrap() as f64;
        assert!((ratio - 1.262838).abs() < 1e-9);
    }

    #[test]
    fn fractional_denominator_scales_both_sides() {
        let (n, d) = normalize_split_ratio(Some(&json!(2)), Some(&json!(0.5))).unwrap();
        assert_eq!((n, d), (Some(2_000_000), Some(500_000)));
    }

    #[test]
    fn null_sides_stay_none() {
        let (n, d) = normalize_split_ratio(None, None).unwrap();
        assert_eq!((n, d), (None, None));
    }

    #[test]
    fn numeric_string_parses() {
        let (n, d) =
            normalize_split_ratio(Some(&json!("3")), Some(&json!("2"))).unwrap();
        assert_eq!((n, d), (Some(3), Some(2)));
    }

    #[test]
    fn negative_float_rejected() {
        // JSON cannot express NaN/Inf literals, so the non-finite guard
        // is defensive. The negative-number guard is reachable via real
        // payloads.
        assert!(normalize_split_ratio(Some(&json!(-1.5)), Some(&json!(1))).is_err());
    }

    #[test]
    fn invalid_string_rejected() {
        assert!(normalize_split_ratio(Some(&json!("not-a-number")), Some(&json!(1))).is_err());
    }
}
