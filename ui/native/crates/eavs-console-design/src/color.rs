//! Color value parsing for scheme slot strings. Schemes may use `#rrggbb`,
//! `#rrggbbaa`, `rgb()/rgba()`, or `oklch(L C H [/ a])`. Everything resolves
//! to sRGB `Rgba` in 0..1 and is emitted as `#rrggbb[aa]`, which the
//! gpui-component theme parser accepts.

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn with_alpha(mut self, a: f32) -> Self {
        self.a = a;
        self
    }

    /// `#rrggbb`, or `#rrggbbaa` when alpha is below 1.
    pub fn to_hex(self) -> String {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.a >= 0.999 {
            format!("#{:02x}{:02x}{:02x}", c(self.r), c(self.g), c(self.b))
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                c(self.r),
                c(self.g),
                c(self.b),
                c(self.a)
            )
        }
    }

    /// Mix toward `other` by `t` in sRGB (0 = self, 1 = other).
    pub fn mix(self, other: Rgba, t: f32) -> Rgba {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Rgba::new(
            l(self.r, other.r),
            l(self.g, other.g),
            l(self.b, other.b),
            l(self.a, other.a),
        )
    }

    /// Relative luminance (WCAG), for picking readable foregrounds.
    pub fn luminance(self) -> f32 {
        let lin = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(self.r) + 0.7152 * lin(self.g) + 0.0722 * lin(self.b)
    }
}

pub fn parse_color(input: &str) -> Result<Rgba> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex).with_context(|| format!("invalid hex color {input:?}"));
    }
    if let Some(body) = s.strip_prefix("oklch(") {
        let body = body
            .strip_suffix(')')
            .ok_or_else(|| anyhow!("unterminated oklch(): {input:?}"))?;
        return parse_oklch(body).with_context(|| format!("invalid oklch color {input:?}"));
    }
    if let Some(body) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb(")) {
        let body = body
            .strip_suffix(')')
            .ok_or_else(|| anyhow!("unterminated rgb(): {input:?}"))?;
        return parse_rgb(body).with_context(|| format!("invalid rgb color {input:?}"));
    }
    bail!("unsupported color syntax {input:?}")
}

fn parse_hex(hex: &str) -> Result<Rgba> {
    let byte = |i: usize| -> Result<f32> {
        let v = u8::from_str_radix(&hex[i..i + 2], 16)?;
        Ok(v as f32 / 255.0)
    };
    match hex.len() {
        6 => Ok(Rgba::new(byte(0)?, byte(2)?, byte(4)?, 1.0)),
        8 => Ok(Rgba::new(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        3 => {
            let n = |i: usize| -> Result<f32> {
                let v = u8::from_str_radix(&hex[i..i + 1], 16)?;
                Ok((v * 17) as f32 / 255.0)
            };
            Ok(Rgba::new(n(0)?, n(1)?, n(2)?, 1.0))
        }
        _ => bail!("expected 3, 6 or 8 hex digits"),
    }
}

fn parse_rgb(body: &str) -> Result<Rgba> {
    let parts: Vec<&str> = body
        .split([',', ' ', '/'])
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 3 {
        bail!("expected at least three components");
    }
    let chan = |s: &str| -> Result<f32> {
        if let Some(p) = s.strip_suffix('%') {
            Ok(p.parse::<f32>()? / 100.0)
        } else {
            Ok(s.parse::<f32>()? / 255.0)
        }
    };
    let alpha = |s: &str| -> Result<f32> {
        if let Some(p) = s.strip_suffix('%') {
            Ok(p.parse::<f32>()? / 100.0)
        } else {
            Ok(s.parse::<f32>()?)
        }
    };
    Ok(Rgba::new(
        chan(parts[0])?,
        chan(parts[1])?,
        chan(parts[2])?,
        parts.get(3).map(|a| alpha(a)).transpose()?.unwrap_or(1.0),
    ))
}

/// `oklch(L C H [/ A])` with L in 0..1 (or percent), C absolute, H degrees.
fn parse_oklch(body: &str) -> Result<Rgba> {
    let (lch, alpha) = match body.split_once('/') {
        Some((lch, a)) => (lch, Some(a.trim())),
        None => (body, None),
    };
    let parts: Vec<&str> = lch.split_whitespace().collect();
    if parts.len() != 3 {
        bail!("expected L C H");
    }
    let l = if let Some(p) = parts[0].strip_suffix('%') {
        p.parse::<f32>()? / 100.0
    } else {
        parts[0].parse::<f32>()?
    };
    let c = parts[1].parse::<f32>()?;
    let h = parts[2]
        .strip_suffix("deg")
        .unwrap_or(parts[2])
        .parse::<f32>()?;
    let a = match alpha {
        Some(a) => {
            if let Some(p) = a.strip_suffix('%') {
                p.parse::<f32>()? / 100.0
            } else {
                a.parse::<f32>()?
            }
        }
        None => 1.0,
    };
    let (r, g, b) = oklch_to_srgb(l, c, h);
    Ok(Rgba::new(r, g, b, a))
}

/// OKLCH -> OKLab -> linear sRGB -> gamma sRGB (Björn Ottosson's matrices).
/// Out-of-gamut values are clamped per channel, which is what browsers do.
fn oklch_to_srgb(l: f32, c: f32, h_deg: f32) -> (f32, f32, f32) {
    let h = h_deg.to_radians();
    let a = c * h.cos();
    let b = c * h.sin();

    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;

    let l3 = l_ * l_ * l_;
    let m3 = m_ * m_ * m_;
    let s3 = s_ * s_ * s_;

    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;

    (gamma(r), gamma(g), gamma(bl))
}

fn gamma(lin: f32) -> f32 {
    let lin = lin.clamp(0.0, 1.0);
    if lin <= 0.003_130_8 {
        12.92 * lin
    } else {
        1.055 * lin.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_color("#222624").unwrap().to_hex(), "#222624");
        assert_eq!(parse_color("#3ba77c80").unwrap().to_hex(), "#3ba77c80");
        assert_eq!(parse_color("#fff").unwrap().to_hex(), "#ffffff");
    }

    #[test]
    fn oklch_white_and_black() {
        assert_eq!(parse_color("oklch(1 0 0)").unwrap().to_hex(), "#ffffff");
        assert_eq!(parse_color("oklch(0 0 0)").unwrap().to_hex(), "#000000");
    }

    #[test]
    fn oklch_known_green_is_greenish() {
        let c = parse_color("oklch(0.470 0.115 163)").unwrap();
        assert!(c.g > c.r && c.g > c.b, "{c:?}");
    }

    #[test]
    fn rgba_with_alpha() {
        let c = parse_color("rgba(255, 255, 255, 0.07)").unwrap();
        assert!((c.a - 0.07).abs() < 1e-6);
        assert_eq!(c.to_hex(), "#ffffff12");
    }
}
