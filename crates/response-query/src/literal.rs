//! Scalar comparison values. Decimal numbers are normalized without floating
//! point conversion, so large integers and tiny decimal differences stay exact.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalarLiteral {
    Null,
    Bool(bool),
    Number(NumberLiteral),
    String(String),
}

impl ScalarLiteral {
    pub(crate) fn parse(raw: &[u8]) -> Result<Self, String> {
        match raw {
            b"null" => Ok(Self::Null),
            b"true" => Ok(Self::Bool(true)),
            b"false" => Ok(Self::Bool(false)),
            [b'"', ..] => serde_json::from_slice(raw)
                .map(Self::String)
                .map_err(|error| format!("invalid JSON string literal: {error}")),
            _ => NumberLiteral::parse(raw).map(Self::Number),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberLiteral {
    negative: bool,
    digits: Vec<u8>,
    exponent: i64,
}

impl NumberLiteral {
    fn parse(raw: &[u8]) -> Result<Self, String> {
        let invalid = || {
            "expected a JSON scalar literal: double-quoted string, number, true, false or null"
                .to_string()
        };
        let negative = raw.first() == Some(&b'-');
        let mut at = usize::from(negative);
        match raw.get(at) {
            Some(b'0') => at += 1,
            Some(b'1'..=b'9') => {
                while raw.get(at).is_some_and(u8::is_ascii_digit) {
                    at += 1;
                }
            }
            _ => return Err(invalid()),
        }
        let mut fraction = 0;
        if raw.get(at) == Some(&b'.') {
            at += 1;
            let start = at;
            while raw.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            fraction = at - start;
            if fraction == 0 {
                return Err(invalid());
            }
        }
        let mantissa_end = at;
        let mut exponent = 0i64;
        if matches!(raw.get(at), Some(b'e' | b'E')) {
            at += 1;
            let start = at;
            if matches!(raw.get(at), Some(b'+' | b'-')) {
                at += 1;
            }
            let digits_start = at;
            while raw.get(at).is_some_and(u8::is_ascii_digit) {
                at += 1;
            }
            if at == digits_start {
                return Err(invalid());
            }
            exponent = std::str::from_utf8(&raw[start..at])
                .map_err(|_| invalid())?
                .parse()
                .map_err(|_| {
                    "numeric literal exponent is outside the signed 64-bit range".to_string()
                })?;
        }
        if at != raw.len() {
            return Err(invalid());
        }
        let mut digits: Vec<_> = raw[..mantissa_end]
            .iter()
            .copied()
            .filter(u8::is_ascii_digit)
            .skip_while(|digit| *digit == b'0')
            .collect();
        if digits.is_empty() {
            return Ok(Self {
                negative: false,
                digits: vec![b'0'],
                exponent: 0,
            });
        }
        let mut trailing = 0;
        while digits.last() == Some(&b'0') {
            digits.pop();
            trailing += 1;
        }
        exponent = exponent
            .checked_sub(fraction as i64)
            .and_then(|value| value.checked_add(trailing))
            .ok_or("normalized numeric literal exponent is outside the signed 64-bit range")?;
        Ok(Self {
            negative,
            digits,
            exponent,
        })
    }
}
