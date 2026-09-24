//! Bounded HTTPS_PROXY selection for the Home WSS connection. Selection is
//! based on the literal configured Home hostname, before any DNS lookup.
use base64::{engine::general_purpose::STANDARD, Engine};
use rustls::pki_types::ServerName;
use std::{env, fmt, net::IpAddr};

const MAX_PROXY_VALUE: usize = 2048;
const MAX_NO_PROXY: usize = 4096;
const MAX_RULES: usize = 64;
const MAX_AUTHORITY: usize = 255;
const MAX_USERINFO: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Error {
    Invalid,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scheme {
    Http,
    Https,
    Socks5,
}

#[derive(Clone)]
pub(super) struct Proxy {
    pub(super) scheme: Scheme,
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) server_name: ServerName<'static>,
    pub(super) basic_auth: Option<String>,
    pub(super) socks_auth: Option<(Vec<u8>, Vec<u8>)>,
}
impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Proxy([redacted])")
    }
}

#[derive(Clone)]
enum HostRule {
    All,
    Ip(IpAddr),
    Cidr(IpAddr, u8),
    Domain { suffix: String, exact: bool },
}
#[derive(Clone)]
struct Rule {
    host: HostRule,
    port: Option<u16>,
}

#[derive(Clone)]
pub(super) struct Policy {
    proxy: Option<Result<Proxy, Error>>,
    no_proxy: Vec<Rule>,
    invalid_no_proxy: bool,
    invalid_env: bool,
}
impl Policy {
    #[cfg(test)]
    pub(super) fn direct() -> Self {
        Self {
            proxy: None,
            no_proxy: Vec::new(),
            invalid_no_proxy: false,
            invalid_env: false,
        }
    }

    /// Go's environment preference: an empty uppercase value falls back to
    /// lowercase. HTTP_PROXY and REQUEST_METHOD do not govern WSS/HTTPS.
    pub(super) fn from_env() -> Self {
        Self::from_env_with(|key| env::var(key))
    }

    fn from_env_with(read: impl Fn(&str) -> Result<String, env::VarError>) -> Self {
        let (upper_proxy, invalid_upper_proxy) = env_value(read("HTTPS_PROXY"));
        let (lower_proxy, invalid_lower_proxy) = env_value(read("https_proxy"));
        let (upper_no_proxy, invalid_upper_no_proxy) = env_value(read("NO_PROXY"));
        let (lower_no_proxy, invalid_lower_no_proxy) = env_value(read("no_proxy"));
        let mut policy =
            Self::from_values(&upper_proxy, &lower_proxy, &upper_no_proxy, &lower_no_proxy);
        let invalid_proxy = invalid_upper_proxy || (upper_proxy.is_empty() && invalid_lower_proxy);
        let invalid_bypass =
            invalid_upper_no_proxy || (upper_no_proxy.is_empty() && invalid_lower_no_proxy);
        if invalid_proxy || (policy.proxy.is_some() && invalid_bypass) {
            policy.invalid_env = true;
        }
        policy
    }

    pub(super) fn from_values(
        upper_proxy: &str,
        lower_proxy: &str,
        upper_no_proxy: &str,
        lower_no_proxy: &str,
    ) -> Self {
        let selected_proxy = if upper_proxy.is_empty() {
            lower_proxy
        } else {
            upper_proxy
        };
        let selected_no_proxy = if upper_no_proxy.is_empty() {
            lower_no_proxy
        } else {
            upper_no_proxy
        };
        let proxy = (!selected_proxy.is_empty()).then(|| parse_proxy(selected_proxy));
        let invalid_no_proxy = selected_no_proxy.len() > MAX_NO_PROXY
            || selected_no_proxy.split(',').count() > MAX_RULES;
        let no_proxy = if invalid_no_proxy {
            Vec::new()
        } else {
            parse_no_proxy(selected_no_proxy)
        };
        Self {
            proxy,
            no_proxy,
            invalid_no_proxy,
            invalid_env: false,
        }
    }

    pub(super) fn select(&self, host: &str, port: u16) -> Result<Option<Proxy>, Error> {
        if self.invalid_env {
            return Err(Error::Invalid);
        }
        if self.proxy.is_none() {
            return Ok(None);
        }
        if self.invalid_no_proxy {
            return Err(Error::Invalid);
        }
        if host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|ip| canonical_ip(ip).is_loopback())
            || self.no_proxy.iter().any(|rule| rule.matches(host, port))
        {
            return Ok(None);
        }
        self.proxy.clone().transpose()
    }
}

fn env_value(value: Result<String, env::VarError>) -> (String, bool) {
    match value {
        Ok(value) => (value, false),
        Err(env::VarError::NotPresent) => (String::new(), false),
        Err(env::VarError::NotUnicode(_)) => (String::new(), true),
    }
}

impl Rule {
    fn matches(&self, host: &str, port: u16) -> bool {
        if self.port.is_some_and(|required| required != port) {
            return false;
        }
        match &self.host {
            HostRule::All => true,
            HostRule::Ip(expected) => host
                .parse::<IpAddr>()
                .is_ok_and(|ip| canonical_ip(ip) == canonical_ip(*expected)),
            HostRule::Cidr(base, bits) => host
                .parse::<IpAddr>()
                .is_ok_and(|ip| matches_cidr(ip, *base, *bits)),
            HostRule::Domain { suffix, exact } => {
                // Go's domain matcher never interprets a literal IP as a
                // domain suffix. Only explicit IP/CIDR rules may bypass it.
                if host.parse::<IpAddr>().is_ok() {
                    return false;
                }
                let host = idna_host(host).unwrap_or_else(|| host.to_ascii_lowercase());
                host.ends_with(suffix) || (*exact && host == suffix[1..])
            }
        }
    }
}

fn matches_cidr(ip: IpAddr, base: IpAddr, bits: u8) -> bool {
    match base {
        IpAddr::V4(base) => {
            let IpAddr::V4(ip) = canonical_ip(ip) else {
                return false;
            };
            let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            (u32::from(ip) & mask) == (u32::from(base) & mask)
        }
        IpAddr::V6(base) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            let network = u128::from(base) & mask;
            // net.ParseCIDR masks the network before IPNet.Contains canonicalizes
            // mapped addresses. Thus ::/0 does not include IPv4, whereas a
            // network still inside ::ffff:0:0/96 does, in either IP spelling.
            match (canonical_ip(ip), canonical_ip(IpAddr::V6(network.into()))) {
                (IpAddr::V4(ip), IpAddr::V4(base)) => {
                    (u32::from(ip) & mask as u32) == u32::from(base)
                }
                (IpAddr::V6(ip), IpAddr::V6(_)) => (u128::from(ip) & mask) == network,
                _ => false,
            }
        }
    }
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}

fn idna_host(host: &str) -> Option<String> {
    if host.is_ascii() {
        Some(host.to_ascii_lowercase())
    } else {
        idna::domain_to_ascii(host).ok()
    }
}

fn parse_no_proxy(raw: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim().to_lowercase();
        if entry.is_empty() {
            continue;
        }
        if entry == "*" {
            return vec![Rule {
                host: HostRule::All,
                port: None,
            }];
        }
        if let Some((ip, bits)) = entry.split_once('/').and_then(|(host, bits)| {
            let ip = host.parse::<IpAddr>().ok()?;
            let bits = bits.parse::<u8>().ok()?;
            let limit = if ip.is_ipv4() { 32 } else { 128 };
            (bits <= limit).then_some((ip, bits))
        }) {
            rules.push(Rule {
                host: HostRule::Cidr(ip, bits),
                port: None,
            });
            continue;
        }
        let Some((host, port)) = split_no_proxy_port(&entry) else {
            continue;
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            rules.push(Rule {
                host: HostRule::Ip(ip),
                port,
            });
            continue;
        }
        // Keep the leading dot so the base domain does not match.
        let host = if host.starts_with("*.") {
            &host[1..]
        } else {
            &host
        };
        if host.is_empty() || host.contains(':') || host.contains('/') {
            continue;
        }
        let exact = !host.starts_with('.');
        let Some(host) = idna_host(host) else {
            // Go retains an invalid Unicode rule literally. It cannot match a
            // validated ASCII endpoint, so ignoring it is equivalent here.
            continue;
        };
        let suffix = if exact { format!(".{host}") } else { host };
        rules.push(Rule {
            host: HostRule::Domain { suffix, exact },
            port,
        });
    }
    rules
}

fn split_no_proxy_port(entry: &str) -> Option<(String, Option<u16>)> {
    if let Some(inner) = entry.strip_prefix('[') {
        let (host, tail) = inner.split_once(']')?;
        if tail.is_empty() {
            return Some((host.to_owned(), None));
        }
        let port = tail.strip_prefix(':')?.parse().ok()?;
        return Some((host.to_owned(), Some(port)));
    }
    if entry.parse::<IpAddr>().is_ok() {
        return Some((entry.to_owned(), None));
    }
    if let Some((host, raw_port)) = entry.rsplit_once(':') {
        if host.contains(':') {
            return None;
        }
        let port = raw_port.parse().ok()?;
        return Some((host.to_owned(), Some(port)));
    }
    Some((entry.to_owned(), None))
}

fn parse_proxy(raw: &str) -> Result<Proxy, Error> {
    if raw.is_empty() || raw.len() > MAX_PROXY_VALUE {
        return Err(Error::Invalid);
    }
    let (scheme, rest) = if raw
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        (Scheme::Http, &raw[7..])
    } else if raw
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        (Scheme::Https, &raw[8..])
    } else if raw
        .get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("socks5://"))
    {
        (Scheme::Socks5, &raw[9..])
    } else if raw
        .get(..10)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("socks5h://"))
    {
        (Scheme::Socks5, &raw[10..])
    } else if raw.contains("://") {
        return Err(Error::Unsupported);
    } else {
        (Scheme::Http, raw)
    };
    let authority = rest.split(['/', '?', '#']).next().ok_or(Error::Invalid)?;
    if authority.is_empty() || authority.len() > MAX_PROXY_VALUE {
        return Err(Error::Invalid);
    }
    let (userinfo, address) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(user, address)| (Some(user), address));
    let (host, port) = split_proxy_authority(address, scheme)?;
    let server_name = ServerName::try_from(host.clone()).map_err(|_| Error::Invalid)?;
    let credentials = userinfo
        .map(|userinfo| {
            if userinfo.len() > MAX_USERINFO {
                return Err(Error::Invalid);
            }
            let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            Ok((decode_percent(user)?, decode_percent(pass)?))
        })
        .transpose()?;
    let (basic_auth, socks_auth) = if scheme == Scheme::Socks5 {
        (None, credentials)
    } else {
        let basic_auth = credentials.map(|(mut user, pass)| {
            user.push(b':');
            user.extend(pass);
            format!("Basic {}", STANDARD.encode(user))
        });
        (basic_auth, None)
    };
    Ok(Proxy {
        scheme,
        host,
        port,
        server_name,
        basic_auth,
        socks_auth,
    })
}

fn split_proxy_authority(address: &str, scheme: Scheme) -> Result<(String, u16), Error> {
    if address.is_empty() || address.len() > MAX_AUTHORITY {
        return Err(Error::Invalid);
    }
    let default_port = match scheme {
        Scheme::Https => 443,
        Scheme::Http => 80,
        Scheme::Socks5 => 1080,
    };
    let (host, port) = if let Some(inner) = address.strip_prefix('[') {
        let (host, tail) = inner.split_once(']').ok_or(Error::Invalid)?;
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| Error::Invalid)?;
        let port = if tail.is_empty() {
            default_port
        } else {
            parse_port(tail.strip_prefix(':').ok_or(Error::Invalid)?)?
        };
        (host, port)
    } else if let Some((host, port)) = address.rsplit_once(':') {
        if host.contains(':') {
            return Err(Error::Invalid);
        }
        (host, parse_port(port)?)
    } else {
        (address, default_port)
    };
    let host = if host.is_ascii() {
        host.to_owned()
    } else {
        idna_host(host).ok_or(Error::Invalid)?
    };
    if host.is_empty()
        || host.len() > MAX_AUTHORITY
        || host.bytes().any(|b| {
            b <= 0x20 || b >= 0x7f || matches!(b, b'@' | b'[' | b']' | b'%' | b'/' | b'\\')
        })
    {
        return Err(Error::Invalid);
    }
    Ok((host, port))
}

fn parse_port(raw: &str) -> Result<u16, Error> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::Invalid);
    }
    raw.parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(Error::Invalid)
}

fn decode_percent(raw: &str) -> Result<Vec<u8>, Error> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes
                .get(i + 1)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or(Error::Invalid)?;
            let lo = bytes
                .get(i + 2)
                .and_then(|b| (*b as char).to_digit(16))
                .ok_or(Error::Invalid)?;
            decoded.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    if decoded.iter().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err(Error::Invalid);
    }
    Ok(decoded)
}

#[cfg(test)]
#[path = "proxy_tests.rs"]
mod tests;
