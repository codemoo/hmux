use super::*;

#[test]
fn unicode_bypass_matches_go_lookup_idna() {
    // Values checked against Go x/net/idna.Lookup.ToASCII, the transform used
    // by net/http's environment-proxy matcher.
    for (input, expected) in [
        ("faß.test", "xn--fa-hia.test"),
        ("ς.test", "xn--3xa.test"),
        ("Σ.test", "xn--4xa.test"),
        ("ｅｘａｍｐｌｅ。TEST", "example.test"),
        ("BÜCHER.test", "xn--bcher-kva.test"),
        ("İ.test", "xn--i-9bb.test"),
        ("straße。TEST", "xn--strae-oqa.test"),
        (".bücher.test", ".xn--bcher-kva.test"),
    ] {
        assert_eq!(idna_host(input).as_deref(), Some(expected), "{input}");
    }
    for (rule, host) in [
        ("bücher.test", "xn--bcher-kva.test"),
        (".bücher.test", "shop.xn--bcher-kva.test"),
        ("faß.test", "xn--fa-hia.test"),
        ("ｅｘａｍｐｌｅ.test", "example.test"),
    ] {
        let policy = Policy::from_values("proxy.test", "", rule, "");
        assert!(
            policy.select(host, 443).unwrap().is_none(),
            "{rule}: {host}"
        );
    }
    assert!(Policy::from_values("proxy.test", "", ".bücher.test", "")
        .select("xn--bcher-kva.test", 443)
        .unwrap()
        .is_some());
}

#[test]
fn selected_non_utf8_environment_values_never_turn_into_direct_connections() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    for selected in ["HTTPS_PROXY", "https_proxy", "NO_PROXY", "no_proxy"] {
        let policy = Policy::from_env_with(|key| {
            if key == selected {
                Err(env::VarError::NotUnicode(OsString::from_vec(vec![0xff])))
            } else if key == "HTTPS_PROXY" && selected != "https_proxy" {
                Ok("http://proxy.test".into())
            } else {
                Err(env::VarError::NotPresent)
            }
        });
        assert_eq!(
            policy.select("gateway.test", 443).err(),
            Some(Error::Invalid),
            "{selected}"
        );
    }
    // An unused lowercase variable cannot override a valid uppercase choice.
    let policy = Policy::from_env_with(|key| match key {
        "HTTPS_PROXY" => Ok("http://proxy.test".into()),
        "https_proxy" => Err(env::VarError::NotUnicode(OsString::from_vec(vec![0xff]))),
        _ => Err(env::VarError::NotPresent),
    });
    assert!(policy.select("gateway.test", 443).unwrap().is_some());
}

#[test]
fn ipv6_networks_do_not_implicitly_bypass_ipv4_destinations() {
    // Expected values independently checked with Go net.ParseCIDR/IPNet.Contains.
    for rule in ["::/0", "::/80", "::ffff:10.0.0.0/80"] {
        let policy = Policy::from_values("proxy.test", "", rule, "");
        for host in ["10.1.2.3", "::ffff:10.1.2.3"] {
            assert!(
                policy.select(host, 443).unwrap().is_some(),
                "{rule}: {host}"
            );
        }
    }
    for rule in ["10.0.0.0/8", "::ffff:10.0.0.0/104", "::ffff:0:0/96"] {
        let policy = Policy::from_values("proxy.test", "", rule, "");
        for host in ["10.1.2.3", "::ffff:10.1.2.3"] {
            assert!(
                policy.select(host, 443).unwrap().is_none(),
                "{rule}: {host}"
            );
        }
        assert!(policy.select("2001:db8::1", 443).unwrap().is_some());
    }
    assert!(Policy::from_values("proxy.test", "", "::/0", "")
        .select("2001:db8::1", 443)
        .unwrap()
        .is_none());
}

#[test]
fn domain_suffix_rules_never_match_literal_ip_addresses() {
    let policy = Policy::from_values("proxy.test", "", ".3.1,*.8,db8::1", "");
    for host in ["10.2.3.1", "10.1.2.8", "2001:db8::1"] {
        assert!(policy.select(host, 443).unwrap().is_some(), "{host}");
    }
    assert!(policy.select("host.3.1", 443).unwrap().is_none());
}

#[test]
fn https_environment_precedence_and_proxy_defaults() {
    let upper = Policy::from_values(
        "http://user:p%40ss@upper.test:8080",
        "https://lower.test",
        "",
        "",
    );
    let selected = upper.select("gateway.test", 443).unwrap().unwrap();
    assert_eq!(
        (selected.scheme, selected.host.as_str(), selected.port),
        (Scheme::Http, "upper.test", 8080)
    );
    assert_eq!(selected.basic_auth.as_deref(), Some("Basic dXNlcjpwQHNz"));
    assert_eq!(format!("{selected:?}"), "Proxy([redacted])");

    let lower = Policy::from_values("", "proxy.test:8888", "", "");
    let selected = lower.select("gateway.test", 443).unwrap().unwrap();
    assert_eq!(
        (selected.scheme, selected.host.as_str(), selected.port),
        (Scheme::Http, "proxy.test", 8888)
    );
    let https = Policy::from_values("HTTPS://proxy.test", "", "", "");
    assert_eq!(
        https.select("gateway.test", 443).unwrap().unwrap().port,
        443
    );
    let http = Policy::from_values("proxy.test", "", "", "");
    assert_eq!(http.select("gateway.test", 443).unwrap().unwrap().port, 80);
    for value in ["socks5://proxy.test", "socks5h://proxy.test"] {
        let proxy = Policy::from_values(value, "", "", "")
            .select("gateway.test", 443)
            .unwrap()
            .unwrap();
        assert_eq!((proxy.scheme, proxy.port), (Scheme::Socks5, 1080));
    }
    let proxy = Policy::from_values("socks5://user:p%40ss@proxy.test", "", "", "")
        .select("gateway.test", 443)
        .unwrap()
        .unwrap();
    assert_eq!(proxy.socks_auth, Some((b"user".to_vec(), b"p@ss".to_vec())));
}

#[test]
fn no_proxy_matches_go_hostname_ip_cidr_and_effective_port() {
    let policy = Policy::from_values(
        "proxy.test:8080",
        "",
        "EXAMPLE.COM,.sub.test,*.wild.test,10.1.0.0/16,[2001:db8::1]:8443,gateway.test:443",
        "lower.test",
    );
    for (host, port) in [
        ("example.com", 443),
        ("api.example.com", 443),
        ("child.sub.test", 443),
        ("child.wild.test", 443),
        ("10.1.2.3", 443),
        ("2001:db8::1", 8443),
        ("gateway.test", 443),
        ("localhost", 9999),
        ("127.0.0.1", 9999),
        ("::1", 9999),
        ("::ffff:127.0.0.1", 9999),
    ] {
        assert!(
            policy.select(host, port).unwrap().is_none(),
            "{host}:{port}"
        );
    }
    for (host, port) in [
        ("sub.test", 443),
        ("wild.test", 443),
        ("10.2.0.1", 443),
        ("2001:db8::1", 443),
        ("gateway.test", 8443),
        ("lower.test", 443),
    ] {
        assert!(
            policy.select(host, port).unwrap().is_some(),
            "{host}:{port}"
        );
    }
    assert!(Policy::from_values("proxy.test", "", "*", "")
        .select("gateway.test", 443)
        .unwrap()
        .is_none());
    // Go applies CIDR to literal URL IPs, not to DNS results of a hostname.
    assert!(Policy::from_values("proxy.test", "", "10.0.0.0/8", "")
        .select("gateway.test", 443)
        .unwrap()
        .is_some());
    for rule in ["10.0.0.0/8", "::ffff:10.0.0.0/104", "10.1.2.3"] {
        assert!(Policy::from_values("proxy.test", "", rule, "")
            .select("::ffff:10.1.2.3", 443)
            .unwrap()
            .is_none());
    }
}

#[test]
fn invalid_or_unsupported_proxy_never_becomes_direct() {
    assert_eq!(
        Policy::from_values("ftp://proxy.test:1080", "", "", "")
            .select("gateway.test", 443)
            .err(),
        Some(Error::Unsupported)
    );
    assert_eq!(
        Policy::from_values("http://proxy.test:0", "", "", "")
            .select("gateway.test", 443)
            .err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        Policy::from_values("http://bad%0Ahost", "", "", "")
            .select("gateway.test", 443)
            .err(),
        Some(Error::Invalid)
    );
    assert_eq!(
        Policy::from_values("proxy.test", "", &"a,".repeat(MAX_RULES + 1), "")
            .select("gateway.test", 443)
            .err(),
        Some(Error::Invalid)
    );
    // A valid bypass does not need to parse or contact an unsupported proxy.
    assert!(
        Policy::from_values("socks5://proxy.test:1080", "", "gateway.test", "")
            .select("gateway.test", 443)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unicode_proxy_authorities_are_normalized_before_dns_and_tls() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        let policy = Policy::from_values(&format!("{scheme}://bücher.test:8443"), "", "", "");
        let proxy = policy.select("gateway.test", 443).unwrap().unwrap();
        assert_eq!(proxy.host, "xn--bcher-kva.test");
        assert_eq!(proxy.port, 8443);
        assert_eq!(
            proxy.server_name,
            ServerName::try_from("xn--bcher-kva.test").unwrap()
        );
    }
    for host in ["bad＠host.test", "bad／host.test", "bad\n호스트.test"] {
        assert!(parse_proxy(&format!("http://{host}")).is_err());
    }
}
