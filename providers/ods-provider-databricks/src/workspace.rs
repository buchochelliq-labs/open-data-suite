//! The workspace a Databricks provider talks to: its `host` setting, read the same way
//! for links (#329) and for signing in (#297, ADR-0021).

/// Why `host` can't be used. The value is never repeated: it may be a pasted URL with a
/// user name and password in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostError {
    /// It isn't set.
    NotConfigured,
    /// It is set, but isn't a workspace URL, and why.
    Invalid(&'static str),
}

/// `https://<host>` from the configured `host`: HTTPS only, no trailing `/`, and no
/// path, query, fragment or user part.
pub(crate) fn https_base(host: Option<&str>) -> Result<String, HostError> {
    let host = host
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or(HostError::NotConfigured)?;
    let rest = match host.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("https") => rest,
        Some(_) => return Err(HostError::Invalid("must be an https:// workspace URL")),
        None => host,
    };
    let authority = rest.trim_end_matches('/');
    if authority.contains(['/', '?', '#']) {
        return Err(HostError::Invalid(
            "must be the workspace's URL alone, with no path or query string",
        ));
    }
    if authority.contains('@') {
        // Never echoed: the part before `@` may be a credential.
        return Err(HostError::Invalid("must not carry a user name or password"));
    }
    let (name, port) = match authority.rsplit_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (authority, None),
    };
    let valid_name = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !name.starts_with(['.', '-'])
        && !name.ends_with(['.', '-']);
    let valid_port = port.is_none_or(|p| !p.is_empty() && p.parse::<u16>().is_ok());
    if !valid_name || !valid_port {
        return Err(HostError::Invalid("isn't a host name"));
    }
    Ok(format!("https://{}", authority.to_ascii_lowercase()))
}
