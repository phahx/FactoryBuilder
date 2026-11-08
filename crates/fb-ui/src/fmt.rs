/// Up to 3 decimals, trailing zeros trimmed: 30, 22.5, 0.333, 1,234.5
pub fn fmt_num(x: f64) -> String {
    if !x.is_finite() {
        return "∞".into();
    }
    let s = format!("{:.3}", x);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    let s = if s == "-0" { "0" } else { s };
    group_thousands(s)
}

pub fn fmt_rate(x: f64) -> String {
    fmt_num((x * 1000.0).round() / 1000.0)
}

/// For editing: up to 4 decimals, no thousands separators.
pub fn fmt_plain(x: f64) -> String {
    let s = format!("{:.4}", x);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Rate with the game's units: "30 per min" for items, "45 m³/min" for fluids.
pub fn fmt_rate_unit(x: f64, form: fb_model::Form) -> String {
    if form.is_fluid() {
        format!("{} m³/min", fmt_rate(x))
    } else {
        format!("{} per min", fmt_rate(x))
    }
}

fn group_thousands(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map(|r| ("-", r)).unwrap_or(("", s));
    let (int, frac) = rest
        .split_once('.')
        .map(|(a, b)| (a, Some(b)))
        .unwrap_or((rest, None));
    if int.len() <= 4 {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    match frac {
        Some(f) => format!("{sign}{out}.{f}"),
        None => format!("{sign}{out}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats() {
        assert_eq!(fmt_num(30.0), "30");
        assert_eq!(fmt_num(22.5), "22.5");
        assert_eq!(fmt_num(1.0 / 3.0), "0.333");
        assert_eq!(fmt_num(92100.0), "92,100");
        assert_eq!(fmt_num(1234.5), "1234.5");
        assert_eq!(fmt_num(-0.0001), "0");
        assert_eq!(fmt_plain(92100.0), "92100");
        assert_eq!(fmt_plain(3.25), "3.25");
        assert_eq!(fmt_rate_unit(30.0, fb_model::Form::Solid), "30 per min");
        assert_eq!(fmt_rate_unit(45.0, fb_model::Form::Liquid), "45 m³/min");
    }
}
