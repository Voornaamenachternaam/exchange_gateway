// src/mapi/encoding.rs
//
// Utility functions for binary MAPI/HTTP encoding and decoding.
// This module provides helpers to safely read/write primitive types from
// a byte slice according to the MS-OXCMAPIHTTP binary format. All functions
// are length‑checked and return a Result with a descriptive error type that
// can be mapped to a `ResponseCode`.

use std::convert::TryFrom;
use thiserror::Error;

/// Errors that can occur while decoding a binary MAPI payload.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("unexpected end of buffer while reading {0}")] UnexpectedEof(&'static str),
    #[error("invalid UTF‑16 string length {0}")] InvalidUtf16Len(usize),
    #[error("invalid UTF‑8 data")] InvalidUtf8,
    #[error("integer overflow while converting {0}")] IntegerOverflow(&'static str),
}

/// Reads a u16 from `buf` at `offset` in little‑endian order.
pub fn read_u16(buf: &[u8], offset: &mut usize) -> Result<u16, DecodeError> {
    if *offset + 2 > buf.len() {
        return Err(DecodeError::UnexpectedEof("u16"));
    }
    let val = u16::from_le_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(val)
}

/// Reads a u32 from `buf` at `offset` in little‑endian order.
pub fn read_u32(buf: &[u8], offset: &mut usize) -> Result<u32, DecodeError> {
    if *offset + 4 > buf.len() {
        return Err(DecodeError::UnexpectedEof("u32"));
    }
    let val = u32::from_le_bytes([
        buf[*offset],
        buf[*offset + 1],
        buf[*offset + 2],
        buf[*offset + 3],
    ]);
    *offset += 4;
    Ok(val)
}

/// Reads a null‑terminated UTF‑16LE string from `buf`.
/// Returns a Rust `String` and advances `offset` past the terminator.
pub fn read_utf16_string(buf: &[u8], offset: &mut usize) -> Result<String, DecodeError> {
    // Find the terminating double zero (little endian UTF‑16 null).
    let mut pos = *offset;
    while pos + 1 < buf.len() {
        if buf[pos] == 0 && buf[pos + 1] == 0 {
            break;
        }
        pos += 2;
    }
    if pos + 1 >= buf.len() {
        return Err(DecodeError::UnexpectedEof("utf16 string"));
    }
    let slice = &buf[*offset..pos];
    // Decode UTF‑16LE to Vec<u16> first.
    let u16_vec: Vec<u16> = slice
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let string = String::from_utf16(&u16_vec).map_err(|_| DecodeError::InvalidUtf16Len(u16_vec.len()))?;
    // Advance past the null terminator.
    *offset = pos + 2;
    Ok(string)
}

/// Writes a u16 into `buf` in little‑endian order.
pub fn write_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend(&val.to_le_bytes());
}

/// Writes a u32 into `buf` in little‑endian order.
pub fn write_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend(&val.to_le_bytes());
}

/// Writes a UTF‑16LE string with a terminating null.
pub fn write_utf16_string(buf: &mut Vec<u8>, s: &str) {
    // Encode as UTF‑16LE.
    let utf16: Vec<u16> = s.encode_utf16().collect();
    for w in utf16 {
        buf.extend(&w.to_le_bytes());
    }
    // Terminator.
    buf.extend(&[0u8, 0u8]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_u16() {
        let mut off = 0usize;
        let mut buf = Vec::new();
        write_u16(&mut buf, 0xABCD);
        let val = read_u16(&buf, &mut off).unwrap();
        assert_eq!(val, 0xABCD);
        assert_eq!(off, 2);
    }

    #[test]
    fn roundtrip_u32() {
        let mut off = 0usize;
        let mut buf = Vec::new();
        write_u32(&mut buf, 0x1234_5678);
        let val = read_u32(&buf, &mut off).unwrap();
        assert_eq!(val, 0x1234_5678);
        assert_eq!(off, 4);
    }

    #[test]
    fn utf16_string_encode_decode() {
        let mut off = 0usize;
        let mut buf = Vec::new();
        let original = "Hello 🌍";
        write_utf16_string(&mut buf, original);
        let decoded = read_utf16_string(&buf, &mut off).unwrap();
        assert_eq!(original, decoded);
    }
}
