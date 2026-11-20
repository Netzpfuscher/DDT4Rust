use std::collections::BTreeMap;

use indexmap::IndexMap;
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};
use serde::Deserialize;

use super::{DataItem, Endian};
use crate::error::{Error, Result};

/// JSON representation of a Data definition (all fields optional, ddt4all omits defaults).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EcuDataSpec {
    #[serde(default)]
    pub bitscount: Option<f64>,
    #[serde(default)]
    pub bytescount: Option<f64>,
    #[serde(default)]
    pub scaled: bool,
    #[serde(default)]
    pub signed: bool,
    #[serde(default)]
    pub byte: bool,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub bytesascii: bool,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub divideby: Option<f64>,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub unit: String,
    #[serde(default)]
    pub comment: String,
    #[serde(default)]
    pub lists: IndexMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct EcuData {
    pub name: String,
    pub bitscount: u32,
    pub bytescount: u32,
    pub scaled: bool,
    pub signed: bool,
    pub byte: bool,
    pub binary: bool,
    pub bytesascii: bool,
    pub step: f64,
    pub offset: f64,
    pub divideby: f64,
    pub format: String,
    pub unit: String,
    pub comment: String,
    pub lists: BTreeMap<i64, String>,
    pub items: IndexMap<String, i64>,
}

impl EcuData {
    pub fn from_spec(name: impl Into<String>, spec: &EcuDataSpec) -> Self {
        let bitscount = spec.bitscount.unwrap_or(8.0).max(1.0) as u32;
        let bytescount = spec.bytescount.unwrap_or(1.0).max(1.0) as u32;
        let mut lists = BTreeMap::new();
        let mut items = IndexMap::new();
        for (k, v) in &spec.lists {
            if let Ok(k) = k.trim().parse::<i64>() {
                lists.insert(k, v.clone());
                items.insert(v.clone(), k);
            }
        }
        Self {
            name: name.into(),
            bitscount,
            bytescount,
            scaled: spec.scaled,
            signed: spec.signed,
            byte: spec.byte,
            binary: spec.binary,
            bytesascii: spec.bytesascii,
            step: spec.step.unwrap_or(1.0),
            offset: spec.offset.unwrap_or(0.0),
            divideby: spec.divideby.unwrap_or(1.0),
            format: spec.format.clone(),
            unit: spec.unit.clone(),
            comment: spec.comment.clone(),
            lists,
            items,
        }
    }

    pub fn data_byte_len(&self) -> usize {
        self.bitscount.div_ceil(8) as usize
    }

    /// Mirror of ddt4all's EcuData.getHexValue(). Returns the byte-swapped hex value
    /// (lowercase, fixed width) or None if the response is too short.
    pub fn get_hex_value(
        &self,
        response: &[u8],
        item: &DataItem,
        endian: Endian,
    ) -> Option<String> {
        let little = match item.endian {
            Some(Endian::Little) => true,
            Some(Endian::Big) => false,
            None => endian == Endian::Little,
        };
        let start_byte = item.firstbyte.checked_sub(1)? as usize;
        let start_bit = item.bitoffset as usize;
        let bits = self.bitscount as usize;
        let data_len = bits.div_ceil(8);
        let needed = (bits + start_bit).div_ceil(8);
        if start_byte + needed > response.len() || needed == 0 {
            return None;
        }
        let slice = &response[start_byte..start_byte + needed];
        if little {
            let raw = BigUint::from_bytes_be(slice);
            let value = (raw >> start_bit) & ((BigUint::one() << bits) - BigUint::one());
            let mut vb = value.to_bytes_be();
            if vb.len() < data_len {
                let mut padded = vec![0u8; data_len - vb.len()];
                padded.extend_from_slice(&vb);
                vb = padded;
            }
            vb.reverse();
            Some(crate::hexutil::to_hex(&vb).to_ascii_lowercase())
        } else {
            let mut value = BigUint::zero();
            for i in 0..bits {
                let bit_index = start_bit + i;
                let bit = (slice[bit_index / 8] >> (7 - (bit_index % 8))) & 1;
                value = (value << 1u32) | BigUint::from(bit);
            }
            let h = format!("{value:x}");
            Some(format!("{:0>width$}", h, width = data_len * 2))
        }
    }

    /// Mirror of ddt4all's EcuData.getDisplayValue().
    pub fn display_value(
        &self,
        response: &[u8],
        item: &DataItem,
        endian: Endian,
    ) -> Option<String> {
        let hex = self.get_hex_value(response, item, endian)?;
        if self.bytesascii {
            let bytes = crate::hexutil::parse_hex(&hex).unwrap_or_default();
            let s = String::from_utf8_lossy(&bytes).replace('\u{FFFD}', "");
            return Some(s);
        }
        let value = BigUint::parse_bytes(hex.as_bytes(), 16).unwrap_or_default();
        if !self.scaled {
            let signed = self.signed_value(&value);
            if let Some(v) = signed {
                if let Some(label) = self.lists.get(&v) {
                    return Some(label.clone());
                }
            } else if let Some(v) = value.to_i64() {
                if let Some(label) = self.lists.get(&v) {
                    return Some(label.clone());
                }
            }
            return Some(hex);
        }
        let mut num = value.to_f64()?;
        if let Some(sv) = self.signed_value(&value) {
            num = sv as f64;
        }
        if self.divideby == 0.0 {
            return None;
        }
        let res = (num * self.step + self.offset) / self.divideby;
        Some(format_value(res, &self.format))
    }

    fn signed_value(&self, value: &BigUint) -> Option<i64> {
        if !self.signed {
            return None;
        }
        let mask: u64 = match self.bytescount {
            1 => 0xFF,
            2 => 0xFFFF,
            _ => return None,
        };
        let raw = (value & BigUint::from(mask)).to_u64()? as i64;
        match self.bytescount {
            1 => Some(-(raw & 0x80) | (raw & 0x7f)),
            2 => Some(-(raw & 0x8000) | (raw & 0x7fff)),
            _ => None,
        }
    }

    pub fn int_value(&self, response: &[u8], item: &DataItem, endian: Endian) -> Option<i64> {
        let hex = self.get_hex_value(response, item, endian)?;
        BigUint::parse_bytes(hex.as_bytes(), 16)?.to_i64()
    }

    /// Mirror of ddt4all's EcuData.setValue(): encode a user value into the request stream.
    /// `value` is either a numeric display value (when scaled) or a hex string.
    pub fn set_value(
        &self,
        value: &str,
        stream: &mut [u8],
        item: &DataItem,
        endian: Endian,
    ) -> Result<()> {
        let bits = self.bitscount as usize;
        let value_bits = if self.bytesascii {
            let mut s: Vec<u8> = value.chars().map(|c| c as u32 as u8).collect();
            s.resize(self.bytescount as usize, b' ');
            s.truncate(self.bytescount as usize);
            let hex = crate::hexutil::to_hex(&s);
            self.write_value_bits(
                &bit_string(
                    &BigUint::parse_bytes(hex.as_bytes(), 16).unwrap_or_default(),
                    bits,
                ),
                stream,
                item,
                endian,
            )?;
            return Ok(());
        } else if self.scaled {
            let v: f64 = value
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("{}: invalid number {value:?}", self.name)))?;
            if self.step == 0.0 {
                return Err(Error::Invalid(format!("{}: step is zero", self.name)));
            }
            let encoded = (v * self.divideby - self.offset) / self.step;
            if !encoded.is_finite() {
                return Err(Error::Invalid(format!("{}: value out of range", self.name)));
            }
            let n = encoded.trunc();
            if n < 0.0 {
                return Err(Error::Invalid(format!(
                    "{}: negative encoded value",
                    self.name
                )));
            }
            bit_string(&BigUint::from(n as u128), bits)
        } else {
            let clean: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            let clean = clean
                .strip_prefix("0x")
                .or_else(|| clean.strip_prefix("0X"))
                .unwrap_or(&clean);
            if clean.is_empty() || !clean.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(Error::Invalid(format!(
                    "{}: invalid hex value {value:?}",
                    self.name
                )));
            }
            let n = BigUint::parse_bytes(clean.as_bytes(), 16).unwrap_or_default();
            bit_string(&n, bits)
        };
        self.write_value_bits(&value_bits, stream, item, endian)
    }

    /// Encode a raw integer, bypassing scaling/ASCII. Used for enum labels.
    pub fn set_raw_value(
        &self,
        raw: &BigUint,
        stream: &mut [u8],
        item: &DataItem,
        endian: Endian,
    ) -> Result<()> {
        let bits = bit_string(raw, self.bitscount as usize);
        self.write_value_bits(&bits, stream, item, endian)
    }

    fn write_value_bits(
        &self,
        value_bits: &[bool],
        stream: &mut [u8],
        item: &DataItem,
        endian: Endian,
    ) -> Result<()> {
        let little = match item.endian {
            Some(Endian::Little) => true,
            Some(Endian::Big) => false,
            None => endian == Endian::Little,
        };
        let start_byte = item
            .firstbyte
            .checked_sub(1)
            .ok_or_else(|| Error::Invalid(format!("data item {} has firstbyte 0", item.name)))?
            as usize;
        let start_bit = item.bitoffset as usize;
        let bits = self.bitscount as usize;
        let needed = (bits + start_bit).div_ceil(8);
        if start_byte + needed > stream.len() {
            return Err(Error::Invalid(format!(
                "request too short for {} (need {} bytes, have {})",
                self.name,
                start_byte + needed,
                stream.len()
            )));
        }

        let mut req_bits: Vec<bool> = (0..needed * 8)
            .map(|i| {
                let b = stream[start_byte + i / 8];
                (b >> (7 - (i % 8))) & 1 == 1
            })
            .collect();

        if little {
            let mut count = 0usize;
            let last_bit = 8usize.saturating_sub(start_bit);
            let first_bit = last_bit.saturating_sub(bits);
            for i in first_bit..last_bit {
                if i >= req_bits.len() || count >= value_bits.len() {
                    break;
                }
                req_bits[i] = value_bits[count];
                count += 1;
            }
            let mut remaining = bits.saturating_sub(count);
            let mut current_byte = 1usize;
            while remaining >= 8 {
                for i in 0..8 {
                    let idx = current_byte * 8 + i;
                    if idx >= req_bits.len() || count >= value_bits.len() {
                        return Err(Error::Invalid(format!("{}: encoding overflow", self.name)));
                    }
                    req_bits[idx] = value_bits[count];
                    count += 1;
                }
                remaining -= 8;
                current_byte += 1;
            }
            if remaining > 0 {
                let first_bit = 8 - remaining;
                for i in first_bit..8 {
                    let idx = current_byte * 8 + i;
                    if idx >= req_bits.len() || count >= value_bits.len() {
                        return Err(Error::Invalid(format!("{}: encoding overflow", self.name)));
                    }
                    req_bits[idx] = value_bits[count];
                    count += 1;
                }
            }
        } else {
            for i in 0..bits {
                if start_bit + i >= req_bits.len() || i >= value_bits.len() {
                    return Err(Error::Invalid(format!("{}: encoding overflow", self.name)));
                }
                req_bits[start_bit + i] = value_bits[i];
            }
        }

        for (i, chunk) in req_bits.chunks(8).enumerate() {
            let mut b = 0u8;
            for (j, bit) in chunk.iter().enumerate() {
                if *bit {
                    b |= 1 << (7 - j);
                }
            }
            stream[start_byte + i] = b;
        }
        Ok(())
    }
}

/// Reproduce Python's `bin(value)[2:].zfill(bits)` truncated to `bits` leading characters.
fn bit_string(value: &BigUint, bits: usize) -> Vec<bool> {
    let s = format!("{value:b}");
    let padded = if s.len() < bits {
        format!("{}{}", "0".repeat(bits - s.len()), s)
    } else {
        s
    };
    padded.chars().take(bits).map(|c| c == '1').collect()
}

/// Reproduce ddt4all's display formatting (fixed decimals if Format has a decimal point).
pub fn format_value(res: f64, format: &str) -> String {
    if let Some((_, frac)) = format.split_once('.') {
        let acc = frac.chars().take_while(|c| c.is_ascii_digit()).count();
        if acc > 0 {
            return format!("{res:.acc$}");
        }
    }
    if res.is_finite() && res.fract() == 0.0 {
        if res >= i64::MIN as f64 && res <= i64::MAX as f64 {
            return format!("{}", res as i64);
        }
        return format!("{res:.0}");
    }
    // Rust's shortest representation is close to Python's repr for the values used here.
    let mut s = format!("{res}");
    if s.contains('e') && !s.contains("e-") && !s.contains("e+") {
        s = s.replace('e', "e+");
    }
    s
}
