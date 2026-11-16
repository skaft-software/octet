//! Non-secret declared endpoint headers.
/// Validate and compose declaration-owned non-secret headers.
pub fn public_headers(
    headers: &'static [(&'static str, &'static str)],
) -> anyhow::Result<http::HeaderMap> {
    let mut output = http::HeaderMap::new();
    for (name, value) in headers {
        if !super::contract::valid_public_header(name, value) {
            anyhow::bail!("provider declaration contains a credential-like or invalid header");
        }
        output.insert(
            http::HeaderName::from_bytes(name.as_bytes())?,
            http::HeaderValue::from_str(value)?,
        );
    }
    Ok(output)
}
