use autoground::Value;

/// Exact agreement on true, false, and unknown. A missing reader value is a
/// failed read and stays out of the rate.
pub fn accuracy(rows: &[(Value, Option<Value>)]) -> Option<f64> {
    let mut matched = 0usize;
    let mut scored = 0usize;
    for (baseline, reader) in rows {
        let Some(reader) = reader else {
            continue;
        };
        scored += 1;
        if *baseline == *reader {
            matched += 1;
        }
    }
    rate(matched, scored)
}

fn rate(matched: usize, scored: usize) -> Option<f64> {
    if scored == 0 {
        None
    } else {
        Some(matched as f64 / scored as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accuracy_is_exact_value_agreement() {
        let rows = [
            (Value::True, Some(Value::True)),
            (Value::Unknown, Some(Value::Unknown)),
            (Value::True, Some(Value::False)),
            (Value::False, None),
        ];
        let rate = accuracy(&rows).unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 1e-12);
        assert!(accuracy(&[(Value::True, None)]).is_none());
    }
}
