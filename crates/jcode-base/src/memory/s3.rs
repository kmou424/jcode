//! Minimal hand-rolled S3 client (Signature V4) for memory sync.
//!
//! Only what the sync engine needs: `HEAD` bucket, `ListObjectsV2`, `GET`,
//! `PUT`, `DELETE`. Hand-rolled because `rust-s3` would pull a large dep tree
//! for four operations; `sha2` is already a dependency and HMAC-SHA256 is a
//! ~15-line RFC 2104 implementation (see `hmac_sha256`, tested against RFC
//! 4231 vectors).

use anyhow::{Context, Result, bail};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::time::Duration;

pub struct S3Client {
    endpoint: String,
    region: String,
    bucket: String,
    /// Leading key prefix inside the bucket ("" = none).
    prefix: String,
    path_style: bool,
    access_key: String,
    secret_key: String,
    http: reqwest::blocking::Client,
}

impl S3Client {
    pub fn from_config(cfg: &crate::config::MemorySyncConfig) -> Result<Self> {
        let resolve = |v: &str| -> Result<String> {
            jcode_provider_env::resolve_secret_value(v)
                .context("failed to resolve memory.sync secret (!{} substitution)")
        };
        let endpoint = cfg.endpoint.trim().trim_end_matches('/').to_string();
        anyhow::ensure!(!endpoint.is_empty(), "memory.sync.endpoint is required");
        anyhow::ensure!(
            endpoint.starts_with("http://") || endpoint.starts_with("https://"),
            "memory.sync.endpoint must be an http(s) URL"
        );
        anyhow::ensure!(
            !cfg.bucket.trim().is_empty(),
            "memory.sync.bucket is required"
        );
        let access_key = resolve(&cfg.access_key)?;
        let secret_key = resolve(&cfg.secret_key)?;
        anyhow::ensure!(
            !access_key.is_empty() && !secret_key.is_empty(),
            "memory.sync access_key/secret_key resolved to empty"
        );
        Ok(Self {
            endpoint,
            region: if cfg.region.trim().is_empty() {
                "us-east-1".to_string()
            } else {
                cfg.region.trim().to_string()
            },
            bucket: cfg.bucket.trim().to_string(),
            prefix: cfg.prefix.trim().trim_matches('/').to_string(),
            path_style: cfg.path_style,
            access_key,
            secret_key,
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    /// Full object key including the configured bucket prefix.
    pub fn full_key(&self, rel: &str) -> String {
        if self.prefix.is_empty() {
            rel.to_string()
        } else {
            format!("{}/{}", self.prefix, rel)
        }
    }

    /// Strip the configured prefix back off a returned object key.
    fn strip_key(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            key.strip_prefix(&format!("{}/", self.prefix))
                .unwrap_or(key)
                .to_string()
        }
    }

    pub fn head_bucket(&self) -> Result<()> {
        let (status, body) = self.request("HEAD", None, &[], &[])?;
        match status.as_u16() {
            200 => Ok(()),
            404 => bail!(
                "memory sync bucket '{}' does not exist (create it beforehand; sync never provisions)",
                self.bucket
            ),
            403 => bail!(
                "memory sync credentials cannot access bucket '{}'",
                self.bucket
            ),
            s => bail!(
                "HEAD bucket '{}' failed: HTTP {} {}",
                self.bucket,
                s,
                String::from_utf8_lossy(&body)
            ),
        }
    }

    /// GET an object; `Ok(None)` on 404.
    pub fn get_object(&self, rel_key: &str) -> Result<Option<Vec<u8>>> {
        let (status, body) = self.request("GET", Some(rel_key), &[], &[])?;
        match status.as_u16() {
            200 => Ok(Some(body)),
            404 => Ok(None),
            s => Err(s3_error("GET", rel_key, s, &body)),
        }
    }

    pub fn put_object(&self, rel_key: &str, body: &[u8]) -> Result<()> {
        let (status, resp) = self.request("PUT", Some(rel_key), &[], body)?;
        if status.is_success() {
            Ok(())
        } else {
            Err(s3_error("PUT", rel_key, status.as_u16(), &resp))
        }
    }

    /// DELETE an object (ops GC only — `entries/` keys are never deleted so
    /// reconcile can always see tombstones).
    pub fn delete_object(&self, rel_key: &str) -> Result<()> {
        let (status, body) = self.request("DELETE", Some(rel_key), &[], &[])?;
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(s3_error("DELETE", rel_key, status.as_u16(), &body))
        }
    }

    /// List all keys under `rel_prefix`, optionally strictly after
    /// `start_after`. Follows continuation tokens; falls back to
    /// `start-after` paging when a server omits the token.
    pub fn list_keys(&self, rel_prefix: &str, start_after: Option<&str>) -> Result<Vec<String>> {
        let mut out: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut first = true;
        loop {
            let mut query: Vec<(&str, String)> = vec![
                ("list-type", "2".to_string()),
                ("prefix", self.full_key(rel_prefix)),
                ("max-keys", "1000".to_string()),
            ];
            match &token {
                Some(t) => query.push(("continuation-token", t.clone())),
                None if first => {
                    if let Some(sa) = start_after {
                        query.push(("start-after", self.full_key(sa)));
                    }
                }
                // No token advertised: page by the last returned key.
                None => match out.last() {
                    Some(last) => query.push(("start-after", self.full_key(last))),
                    None => break,
                },
            }
            let query_refs: Vec<(&str, &str)> =
                query.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let (status, body) = self.request("GET", None, &query_refs, &[])?;
            if !status.is_success() {
                return Err(s3_error("LIST", rel_prefix, status.as_u16(), &body));
            }
            let xml = String::from_utf8_lossy(&body);
            let page_keys = xml_tag_values(&xml, "Key");
            out.extend(page_keys.iter().map(|k| self.strip_key(k)));
            if xml_tag_value(&xml, "IsTruncated").as_deref() != Some("true") {
                break;
            }
            if page_keys.is_empty() {
                break; // truncated but nothing to page from: stop safely
            }
            token = xml_tag_value(&xml, "NextContinuationToken").filter(|t| !t.is_empty());
            first = false;
        }
        Ok(out)
    }

    fn request(
        &self,
        method: &str,
        rel_key: Option<&str>,
        query: &[(&str, &str)],
        body: &[u8],
    ) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let key = rel_key.map(|k| self.full_key(k)).unwrap_or_default();
        let (url, canonical_uri, host) = self.build_url(&key);
        let now = Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let payload_hash = hex::encode(Sha256::digest(body));

        let canonical_query = canonical_query_string(query);
        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            host, payload_hash, amz_date
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, canonical_uri, canonical_query, canonical_headers, signed_headers, payload_hash
        );
        let scope = format!("{}/{}/s3/aws4_request", date_stamp, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date,
            scope,
            hex::encode(Sha256::digest(canonical_request.as_bytes()))
        );
        let signature = self.signature(&date_stamp, &string_to_sign);
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key, scope, signed_headers, signature
        );

        let mut full_url = url;
        if !canonical_query.is_empty() {
            full_url.push('?');
            full_url.push_str(&canonical_query);
        }
        let resp = self
            .http
            .request(method.parse().unwrap_or(reqwest::Method::GET), &full_url)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("Authorization", &auth)
            .body(body.to_vec())
            .send()?;
        let status = resp.status();
        let bytes = resp.bytes()?.to_vec();
        Ok((status, bytes))
    }

    /// (url, canonical_uri, host[:port]) for a full object key ("" = bucket root).
    fn build_url(&self, key: &str) -> (String, String, String) {
        let base = reqwest::Url::parse(&self.endpoint)
            .unwrap_or_else(|_| reqwest::Url::parse("http://invalid").unwrap());
        let host = base.host_str().unwrap_or_default().to_string();
        let port = base.port().map(|p| format!(":{}", p)).unwrap_or_default();
        let host_header = format!("{}{}", host, port);
        if self.path_style {
            let uri = if key.is_empty() {
                format!("/{}", self.bucket)
            } else {
                format!("/{}/{}", self.bucket, uri_encode_path(key))
            };
            (format!("{}{}", self.endpoint, uri), uri, host_header)
        } else {
            let uri = if key.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", uri_encode_path(key))
            };
            (
                format!("{}://{}.{}{}", base.scheme(), self.bucket, host, port) + uri.as_str(),
                uri,
                format!("{}.{}{}", self.bucket, host, port),
            )
        }
    }

    fn signature(&self, date_stamp: &str, string_to_sign: &str) -> String {
        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()))
    }
}

/// RFC 2104 HMAC-SHA256 (block size 64).
fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    const B: usize = 64;
    let mut k = [0u8; B];
    if key.len() > B {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    for b in k.iter() {
        inner.update([*b ^ 0x36]);
    }
    inner.update(msg);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    for b in k.iter() {
        outer.update([*b ^ 0x5c]);
    }
    outer.update(inner_hash);
    outer.finalize().to_vec()
}

/// SigV4 percent-encoding: unreserved chars pass through; '/' kept when
/// encoding a path (per-segment encoding preserving separators).
fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn uri_encode_path(s: &str) -> String {
    uri_encode(s, true)
}

fn canonical_query_string(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

/// First text content of `<tag>` in a flat XML document (unescaped).
fn xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    xml_tag_values(xml, tag).into_iter().next()
}

fn xml_tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else { break };
        out.push(xml_unescape(&after[..end]));
        rest = &after[end + close.len()..];
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn s3_error(op: &str, key: &str, status: u16, body: &[u8]) -> anyhow::Error {
    let text = String::from_utf8_lossy(body);
    let msg = xml_tag_value(&text, "Message").unwrap_or_else(|| text.chars().take(300).collect());
    anyhow::anyhow!("S3 {} '{}' failed: HTTP {} {}", op, key, status, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc4231_case2() {
        let out = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(out),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_long_key_hashed_first() {
        // RFC 4231 test case 6 (131-byte key of 0xaa).
        let out = hmac_sha256(
            &[0xaau8; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex::encode(out),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn uri_encoding_rules() {
        assert_eq!(uri_encode("a/b c&d", true), "a/b%20c%26d");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode("k-_~.x", false), "k-_~.x");
    }

    #[test]
    fn canonical_query_sorted_and_encoded() {
        let q = canonical_query_string(&[
            ("prefix", "ops/global/"),
            ("list-type", "2"),
            ("start-after", "ops/global/x y"),
        ]);
        assert_eq!(
            q,
            "list-type=2&prefix=ops%2Fglobal%2F&start-after=ops%2Fglobal%2Fx%20y"
        );
    }

    #[test]
    fn parses_list_response() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult><Name>b</Name><Prefix>ops/</Prefix>
<KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys><IsTruncated>true</IsTruncated>
<NextContinuationToken>tok&amp;1</NextContinuationToken>
<Contents><Key>ops/global/a.json</Key></Contents>
<Contents><Key>ops/global/b.json</Key></Contents>
</ListBucketResult>"#;
        assert_eq!(
            xml_tag_values(xml, "Key"),
            vec![
                "ops/global/a.json".to_string(),
                "ops/global/b.json".to_string()
            ]
        );
        assert_eq!(xml_tag_value(xml, "IsTruncated"), Some("true".into()));
        assert_eq!(
            xml_tag_value(xml, "NextContinuationToken"),
            Some("tok&1".into())
        );
    }
}
