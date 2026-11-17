//! Descriptor-declared geometry. Logical ranges alone do not establish shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub logical_min: i32,
    pub logical_max: i32,
    pub physical_min: i32,
    pub physical_max: i32,
    pub units: u32,
    pub exponent: u32,
    pub bits: u16,
}
impl Axis {
    pub fn length_mm(self) -> Result<f64, String> {
        let unit_mm = match self.units {
            0x11 => 10.0,
            0x13 => 25.4,
            _ => return Err(format!("unsupported HID length unit 0x{:X}", self.units)),
        };
        let exp = if self.exponent <= 15 {
            ((self.exponent as i32) << 28) >> 28
        } else {
            self.exponent as i32
        };
        if !(-8..=7).contains(&exp)
            || self.physical_max <= self.physical_min
            || self.logical_max <= self.logical_min
        {
            return Err("invalid HID physical/logical range or unit exponent".into());
        }
        let length =
            (self.physical_max as f64 - self.physical_min as f64) * 10f64.powi(exp) * unit_mm;
        if !length.is_finite() || length <= 0.0 {
            return Err("invalid physical length".into());
        }
        Ok(length)
    }
    pub fn counts_per_mm(self) -> Result<f64, String> {
        Ok((self.logical_max as f64 - self.logical_min as f64) / self.length_mm()?)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub x: Axis,
    pub y: Axis,
}
impl Geometry {
    pub fn label(self) -> Result<String, String> {
        Ok(format!(
            "{:.1} x {:.1} mm; W/H {:.4}\nX {}..{} / Y {}..{}; {:.0}/{:.0} DPI",
            self.x.length_mm()?,
            self.y.length_mm()?,
            self.aspect()?,
            self.x.logical_min,
            self.x.logical_max,
            self.y.logical_min,
            self.y.logical_max,
            self.x.counts_per_mm()? * 25.4,
            self.y.counts_per_mm()? * 25.4
        ))
    }
    pub fn aspect(self) -> Result<f64, String> {
        Ok(self.x.length_mm()? / self.y.length_mm()?)
    }
    pub fn description(self) -> Result<String, String> {
        Ok(format!("HID surface {:.1} x {:.1} mm (W/H {:.4}); X {}..{} / Y {}..{}; {:.2}/{:.2} counts/mm ({:.0}/{:.0} DPI), {} / {} bits",
            self.x.length_mm()?,self.y.length_mm()?,self.aspect()?,self.x.logical_min,self.x.logical_max,self.y.logical_min,self.y.logical_max,
            self.x.counts_per_mm()?,self.y.counts_per_mm()?,self.x.counts_per_mm()?*25.4,self.y.counts_per_mm()?*25.4,self.x.bits,self.y.bits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn axis(units: u32, exponent: u32, physical_max: i32) -> Axis {
        Axis {
            units,
            exponent,
            physical_min: 0,
            physical_max,
            logical_min: 0,
            logical_max: 10000,
            bits: 16,
        }
    }
    #[test]
    fn physical_units_and_exponents_determine_shape_not_logical_range() {
        let x = axis(0x11, 0xE, 1400);
        let y = axis(0x13, (-3i32) as u32, 3500);
        assert!((x.length_mm().unwrap() - 140.0).abs() < 1e-9);
        assert!((y.length_mm().unwrap() - 88.9).abs() < 1e-9);
        assert!((Geometry { x, y }.aspect().unwrap() - 140.0 / 88.9).abs() < 1e-9);
        assert_eq!(x.logical_max, y.logical_max); // Square logical coordinates, non-square surface.
    }
    #[test]
    fn missing_units_or_physical_size_cannot_be_guessed() {
        assert!(axis(0, 0, 100).length_mm().is_err());
        assert!(axis(0x11, 0, 0).length_mm().is_err());
        assert!(axis(0x1001, 0, 100).length_mm().is_err());
        assert!(axis(0x11, 32, 100).length_mm().is_err());
    }
}
