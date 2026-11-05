use aes::cipher::{BlockModeEncrypt, KeyInit, KeyIvInit, block_padding::Pkcs7};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::Digest;
use std::io::{Read, Write};

const MAX: usize = 32_000_000;
fn bytes(v: &Value, key: &str) -> Result<Vec<u8>> {
    let a = v[key].as_array().context("Expected byte array")?;
    ensure!(a.len() <= MAX, "Byte buffer too large");
    a.iter()
        .map(|n| {
            n.as_u64()
                .filter(|n| *n <= 255)
                .map(|n| n as u8)
                .context("Invalid byte")
        })
        .collect()
}
pub fn call(op: &str, args: &str) -> String {
    let result = (|| {
        let args: Value = serde_json::from_str(args)?;
        native(op, &args)
    })();
    match result {
        Ok(value) => json!({"value":value}).to_string(),
        Err(error) => json!({"error":error.to_string()}).to_string(),
    }
}
fn native(op: &str, a: &Value) -> Result<Value> {
    Ok(match op {
        "encode" => {
            let text = a["value"].as_str().unwrap_or_default();
            ensure!(text.len() <= MAX, "Text too large");
            let output = match a["encoding"].as_str().unwrap_or("utf8") {
                "base64" => STANDARD.decode(text)?,
                "hex" => {
                    ensure!(
                        text.is_ascii()
                            && text.len().is_multiple_of(2)
                            && text.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "Invalid hex"
                    );
                    (0..text.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(Into::into))
                        .collect::<Result<Vec<_>>>()?
                }
                "binary" | "latin1" | "ascii" => text.chars().map(|c| c as u32 as u8).collect(),
                "utf16le" | "ucs2" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                "utf8" | "utf-8" => text.as_bytes().to_vec(),
                _ => anyhow::bail!("Unsupported byte encoding"),
            };
            json!(output)
        }
        "decode" => {
            let value = bytes(a, "value")?;
            json!(match a["encoding"].as_str().unwrap_or("utf8") {
                "base64" => STANDARD.encode(value),
                "hex" => value.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "binary" | "latin1" => value.iter().map(|b| char::from(*b)).collect(),
                "ascii" => value.iter().map(|b| char::from(*b & 127)).collect(),
                "utf16le" | "ucs2" => String::from_utf16_lossy(
                    &value
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect::<Vec<_>>()
                ),
                "utf8" | "utf-8" => String::from_utf8_lossy(&value).into_owned(),
                _ => anyhow::bail!("Unsupported byte encoding"),
            })
        }
        "md5" => json!(
            md5::Md5::digest(bytes(a, "value")?)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ),
        "random" => {
            let size = a["size"].as_u64().context("Invalid random byte size")? as usize;
            ensure!(size <= 1_000_000, "Random byte limit exceeded");
            let mut buf = vec![0; size];
            rand::fill(&mut buf[..]);
            json!(buf)
        }
        "aes" => {
            let value = bytes(a, "value")?;
            let key = bytes(a, "key")?;
            let iv = bytes(a, "iv")?;
            let mode = a["mode"].as_str().unwrap_or_default().to_lowercase();
            macro_rules! cbc {
                ($cipher:ty) => {
                    cbc::Encryptor::<$cipher>::new_from_slices(&key, &iv)
                        .map_err(|_| anyhow::anyhow!("Invalid AES key/IV"))?
                        .encrypt_padded_vec::<Pkcs7>(&value)
                };
            }
            macro_rules! ecb {
                ($cipher:ty) => {
                    ecb::Encryptor::<$cipher>::new_from_slice(&key)
                        .map_err(|_| anyhow::anyhow!("Invalid AES key"))?
                        .encrypt_padded_vec::<Pkcs7>(&value)
                };
            }
            json!(match mode.as_str() {
                "aes-128-cbc" => cbc!(aes::Aes128),
                "aes-192-cbc" => cbc!(aes::Aes192),
                "aes-256-cbc" => cbc!(aes::Aes256),
                "aes-128-ecb" => ecb!(aes::Aes128),
                "aes-192-ecb" => ecb!(aes::Aes192),
                "aes-256-ecb" => ecb!(aes::Aes256),
                _ => anyhow::bail!("Unsupported AES mode"),
            })
        }
        "rsa" => {
            use rsa::{
                RsaPublicKey, pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey,
                traits::PublicKeyParts,
            };
            let pem = a["key"].as_str().context("Invalid RSA key")?;
            let key = RsaPublicKey::from_public_key_pem(pem)
                .or_else(|_| RsaPublicKey::from_pkcs1_pem(pem))?;
            let data = bytes(a, "value")?;
            ensure!(
                data.len() <= 128 && key.size() == 128,
                "LX RSA requires at most 128 bytes and a 1024-bit key"
            );
            let n = num_bigint::BigUint::from_bytes_be(&key.n().to_bytes_be());
            let e = num_bigint::BigUint::from_bytes_be(&key.e().to_bytes_be());
            let value = num_bigint::BigUint::from_bytes_be(&data);
            ensure!(value < n, "RSA input exceeds modulus");
            let encrypted = value.modpow(&e, &n).to_bytes_be();
            let mut output = vec![0; 128 - encrypted.len()];
            output.extend(encrypted);
            json!(output)
        }
        "deflate" => {
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&bytes(a, "value")?)?;
            json!(encoder.finish()?)
        }
        "inflate" => {
            let data = bytes(a, "value")?;
            let mut decoder = flate2::read::ZlibDecoder::new(&data[..]).take(MAX as u64 + 1);
            let mut output = Vec::new();
            decoder.read_to_end(&mut output)?;
            ensure!(output.len() <= MAX, "Inflated data exceeds limit");
            json!(output)
        }
        _ => anyhow::bail!("Unsupported native operation"),
    })
}
