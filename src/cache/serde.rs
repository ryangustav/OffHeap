use napi::{Env, JsBuffer, JsString, JsUnknown, Result, ValueType};

pub(super) const TAG_BUFFER:   u8 = 1;
pub(super) const TAG_STRING:   u8 = 2;
pub(super) const TAG_JSON:     u8 = 3;
pub(super) const TAG_I64:      u8 = 4;
pub(super) const TAG_LZ4_JSON: u8 = 5;

pub(super) fn serialize_value(
    compression: bool,
    _env: Env,
    value: JsUnknown,
    force_compression: Option<bool>,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let value_type = value.get_type()?;

    if value.is_buffer()? {
        let buf = JsBuffer::try_from(value)?;
        let raw_bytes: Vec<u8> = buf.into_value()?.to_vec();
        bytes.push(TAG_BUFFER);
        bytes.extend_from_slice(&raw_bytes);
    } else if value_type == ValueType::String {
        let js_str = JsString::try_from(value)?;
        let utf8 = js_str.into_utf8()?;
        let slice = utf8.as_slice();
        if slice.starts_with(b"\0J") {
            let compress = force_compression.unwrap_or(compression);
            if compress {
                bytes.push(TAG_LZ4_JSON);
                let compressed = lz4_flex::compress_prepend_size(&slice[2..]);
                bytes.extend_from_slice(&compressed);
            } else {
                bytes.push(TAG_JSON);
                bytes.extend_from_slice(&slice[2..]);
            }
        } else {
            bytes.push(TAG_STRING);
            bytes.extend_from_slice(slice);
        }
    } else if value_type == ValueType::Number {
        let num = value.coerce_to_number()?;
        let val = num.get_double()?;
        if val.fract() == 0.0 && val >= (i64::MIN as f64) && val <= (i64::MAX as f64) {
            bytes.push(TAG_I64);
            bytes.extend_from_slice(&(val as i64).to_le_bytes());
        } else {
            let s = val.to_string();
            let compress = force_compression.unwrap_or(compression);
            if compress {
                bytes.push(TAG_LZ4_JSON);
                let compressed = lz4_flex::compress_prepend_size(s.as_bytes());
                bytes.extend_from_slice(&compressed);
            } else {
                bytes.push(TAG_JSON);
                bytes.extend_from_slice(s.as_bytes());
            }
        }
    } else {
        return Err(napi::Error::new(napi::Status::InvalidArg, "Complex types must be serialized to JSON in JS wrapper"));
    }
    Ok(bytes)
}

pub(super) fn deserialize_value(
    max_bytes: Option<usize>,
    env: Env,
    bytes: Vec<u8>,
) -> Result<JsUnknown> {
    if bytes.is_empty() {
        return Ok(env.get_undefined()?.into_unknown());
    }
    let tag = bytes[0];
    let payload = &bytes[1..];
    match tag {
        TAG_BUFFER => {
            let js_buf = env.create_buffer_copy(payload)?;
            Ok(js_buf.into_unknown())
        }
        TAG_STRING => {
            let s = std::str::from_utf8(payload)
                .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
            let js_str = env.create_string(s)?;
            Ok(js_str.into_unknown())
        }
        TAG_JSON => {
            let s = std::str::from_utf8(payload)
                .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
            let mut prefixed = String::with_capacity(2 + s.len());
            prefixed.push('\0');
            prefixed.push('J');
            prefixed.push_str(s);
            let js_str = env.create_string(&prefixed)?;
            Ok(js_str.into_unknown())
        }
        TAG_I64 => {
            if payload.len() == 8 {
                let bytes: [u8; 8] = payload.try_into()
                    .map_err(|_| napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"))?;
                let val = i64::from_le_bytes(bytes);
                let js_num = env.create_double(val as f64)?;
                Ok(js_num.into_unknown())
            } else {
                Err(napi::Error::new(napi::Status::InvalidArg, "Counter value is corrupted"))
            }
        }
        TAG_LZ4_JSON => {
            if payload.len() < 4 {
                return Err(napi::Error::new(napi::Status::InvalidArg, "Compressed payload is too short"));
            }
            let size_bytes: [u8; 4] = payload[0..4].try_into()
                .map_err(|_| napi::Error::new(napi::Status::InvalidArg, "Invalid compressed header"))?;
            let uncompressed_size = u32::from_le_bytes(size_bytes) as usize;

            // Enforce safety limit: min(32 MB, max_bytes * 10%) to prevent decompression bombs.
            let mut limit = 32 * 1024 * 1024;
            if let Some(mb) = max_bytes {
                limit = limit.min((mb as f64 * 0.1) as usize).max(1024);
            }
            if uncompressed_size > limit {
                return Err(napi::Error::new(
                    napi::Status::InvalidArg,
                    format!("Decompressed size {} exceeds safety limit of {} bytes", uncompressed_size, limit)
                ));
            }

            let mut decompressed = vec![0u8; uncompressed_size];
            lz4_flex::block::decompress_into(&payload[4..], &mut decompressed)
                .map_err(|e| napi::Error::new(napi::Status::GenericFailure, format!("LZ4 Decompression failed: {}", e)))?;

            let s = std::str::from_utf8(&decompressed)
                .map_err(|e| napi::Error::new(napi::Status::StringExpected, e.to_string()))?;
            let mut prefixed = String::with_capacity(2 + s.len());
            prefixed.push('\0');
            prefixed.push('J');
            prefixed.push_str(s);
            let js_str = env.create_string(&prefixed)?;
            Ok(js_str.into_unknown())
        }
        _ => Err(napi::Error::new(napi::Status::InvalidArg, "Invalid data type tag in cache storage")),
    }
}
