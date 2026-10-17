//! Go-syntax duration newtype (parity with Go's `time.ParseDuration`).

use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A duration written in Go syntax, e.g. `250ms`, `1h30m`, `1.5s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoDuration(pub Duration);

impl GoDuration {
    pub fn as_std(&self) -> Duration {
        self.0
    }
}

impl FromStr for GoDuration {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid duration {input:?}");
        let mut s = input;
        let mut negative = false;
        if let Some(rest) = s.strip_prefix(['-', '+']) {
            negative = s.starts_with('-');
            s = rest;
        }
        if s == "0" {
            return Ok(GoDuration(Duration::ZERO));
        }
        if s.is_empty() {
            return Err(invalid());
        }

        let mut total: u128 = 0;
        while !s.is_empty() {
            // Integer part.
            let int_len = s.bytes().take_while(u8::is_ascii_digit).count();
            let (int_str, rest) = s.split_at(int_len);
            s = rest;
            // Optional fractional part.
            let mut frac_str = "";
            if let Some(rest) = s.strip_prefix('.') {
                let frac_len = rest.bytes().take_while(u8::is_ascii_digit).count();
                (frac_str, s) = rest.split_at(frac_len);
            }
            if int_str.is_empty() && frac_str.is_empty() {
                return Err(invalid());
            }
            // Unit.
            let unit_len = s
                .find(|c: char| c == '.' || c.is_ascii_digit())
                .unwrap_or(s.len());
            let (unit, rest) = s.split_at(unit_len);
            s = rest;
            let unit_nanos: u128 = match unit {
                "ns" => 1,
                "us" | "µs" | "μs" => 1_000,
                "ms" => 1_000_000,
                "s" => 1_000_000_000,
                "m" => 60 * 1_000_000_000,
                "h" => 3600 * 1_000_000_000,
                _ => return Err(invalid()),
            };

            let int_val: u128 = if int_str.is_empty() {
                0
            } else {
                int_str.parse().map_err(|_| invalid())?
            };
            let mut nanos = int_val.checked_mul(unit_nanos).ok_or_else(invalid)?;
            // Fraction: truncate to 18 digits to keep u128 math safe.
            let frac_digits = &frac_str[..frac_str.len().min(18)];
            if !frac_digits.is_empty() {
                let frac_val: u128 = frac_digits.parse().map_err(|_| invalid())?;
                let scale = 10u128.pow(frac_digits.len() as u32);
                nanos = nanos
                    .checked_add(frac_val * unit_nanos / scale)
                    .ok_or_else(invalid)?;
            }
            total = total.checked_add(nanos).ok_or_else(invalid)?;
        }

        if negative && total != 0 {
            return Err(format!(
                "invalid duration {input:?}: negative durations are not supported"
            ));
        }
        let secs = u64::try_from(total / 1_000_000_000).map_err(|_| invalid())?;
        let subsec = (total % 1_000_000_000) as u32;
        Ok(GoDuration(Duration::new(secs, subsec)))
    }
}

impl<'de> Deserialize<'de> for GoDuration {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for GoDuration {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{}ns", self.0.as_nanos()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Duration, String> {
        s.parse::<GoDuration>().map(|d| d.as_std())
    }

    #[test]
    fn parses_valid_table() {
        let cases = [
            ("250ms", Duration::from_millis(250)),
            ("2m", Duration::from_secs(120)),
            ("1h30m", Duration::from_secs(5400)),
            ("0", Duration::ZERO),
            ("500µs", Duration::from_micros(500)),
            ("500us", Duration::from_micros(500)),
            ("7ns", Duration::from_nanos(7)),
            ("1.5h", Duration::from_secs(5400)),
            (".5s", Duration::from_millis(500)),
            ("+3s", Duration::from_secs(3)),
            ("-0", Duration::ZERO),
            ("1m30.5s", Duration::from_millis(90_500)),
        ];
        for (input, want) in cases {
            assert_eq!(parse(input), Ok(want), "input {input:?}");
        }
    }

    #[test]
    fn rejects_invalid() {
        for input in [
            "5 minutes", "", "5", "s", "1x", "1s junk", "-", "1.s5", "--1s", "-1s", "1..5s",
        ] {
            assert!(parse(input).is_err(), "input {input:?} should fail");
        }
    }

    #[test]
    fn serde_roundtrip_via_yaml() {
        let d: GoDuration = serde_yaml_ng::from_str("\"1h30m\"").unwrap();
        assert_eq!(d.as_std(), Duration::from_secs(5400));
        let out = serde_yaml_ng::to_string(&d).unwrap();
        let back: GoDuration = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(back, d);
    }
}
