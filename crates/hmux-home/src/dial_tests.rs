use super::*;
use hmux_protocol::{
    protobuf::{self, Negotiated},
    transport,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    ServerConfig,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Condvar, Mutex,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

static TEST_SOCKET: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn cert_for(host: &str, expired: bool) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![host.to_owned()]).unwrap();
    if expired {
        params.not_before = rcgen::date_time_ymd(2019, 1, 1);
        params.not_after = rcgen::date_time_ymd(2020, 1, 1);
    }
    let cert = params.self_signed(&key).unwrap();
    (
        cert.der().clone(),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
    )
}
fn ca_signed_for(
    host: &str,
) -> (
    CertificateDer<'static>,
    CertificateDer<'static>,
    PrivateKeyDer<'static>,
) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec![host.to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &ca, &ca_key)
        .unwrap();
    (
        ca.der().clone(),
        leaf.der().clone(),
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    )
}
fn server_config(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> Arc<ServerConfig> {
    Arc::new(
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap(),
    )
}
fn client(cert: CertificateDer<'static>, address: SocketAddr) -> Client {
    let resolver: Arc<Resolver> = Arc::new(move |host, port| {
        assert_eq!(host, "gateway.test");
        assert_eq!(port, address.port());
        Ok(vec![address])
    });
    Client::with_config(config_from_roots(vec![cert]).unwrap(), resolver)
}
fn endpoint(address: SocketAddr) -> String {
    format!("wss://gateway.test:{}/connect", address.port())
}
fn request_key(request: &str) -> &str {
    request
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key")
                .then_some(value.trim())
        })
        .unwrap()
}
async fn read_request<S: AsyncRead + Unpin>(socket: &mut S) -> String {
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while request.len() < 8192 {
        let size = socket.read(&mut buf).await.unwrap_or(0);
        if size == 0 {
            break;
        }
        request.extend_from_slice(&buf[..size]);
        if request.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(request).unwrap()
}

fn proxied_client(
    roots: Vec<CertificateDer<'static>>,
    proxy_address: SocketAddr,
    policy: &str,
) -> Client {
    Client::with_config_and_policy(
        config_from_roots(roots).unwrap(),
        Arc::new(move |host, port| {
            assert_eq!(host, "proxy.test");
            assert_eq!(port, proxy_address.port());
            Ok(vec![proxy_address])
        }),
        Arc::new(proxy::Policy::from_values(policy, "", "", "")),
    )
}
async fn ws_server(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    selected: Option<&'static str>,
) -> (SocketAddr, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config(cert, key));
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut socket = acceptor.accept(socket).await.unwrap();
        assert_eq!(socket.get_ref().1.server_name(), Some("gateway.test"));
        let request = read_request(&mut socket).await;
        let accept = tokio_tungstenite::tungstenite::handshake::derive_accept_key(
            request_key(&request).as_bytes(),
        );
        let protocol = selected
            .map(|value| format!("Sec-WebSocket-Protocol: {value}\r\n"))
            .unwrap_or_default();
        let response = format!("HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n{protocol}\r\n");
        socket.write_all(response.as_bytes()).await.unwrap();
        let mut buf = [0u8; 256];
        while socket.read(&mut buf).await.unwrap_or(0) != 0 {}
        request
    });
    (address, task)
}
async fn close(connection: transport::Connection) {
    let transport::Connection {
        sender,
        reader,
        task,
    } = connection;
    sender.close();
    drop(reader);
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn exact_endpoint_and_redaction() {
    for good in [
        "wss://gateway.test/connect",
        "wss://gateway.test:8443/connect",
        "wss://[::1]/connect",
        "wss://[::1]:444/connect",
        "wss://127.0.0.1:1/connect",
    ] {
        let parsed = Endpoint::parse(good).unwrap();
        assert_eq!(format!("{parsed:?}"), "Endpoint([redacted])");
    }
    for bad in [
        "https://gateway.test/connect",
        "WSS://gateway.test/connect",
        "wss://gateway.test/",
        "wss://gateway.test//connect",
        "wss://gateway.test/%63onnect",
        "wss://gateway.test/connect?x=1",
        "wss://gateway.test/connect#frag",
        "wss://a@b/connect",
        "wss://gateway.test:abc/connect",
        "wss://gateway.test:99999/connect",
        "wss://gateway.test:0/connect",
        "wss://gateway.test:/connect",
        "wss://[::1]garbage/connect",
        "wss://::1/connect",
        "wss://gateway.test/../connect",
        "wss://gateway.test\\connect",
    ] {
        assert!(matches!(Endpoint::parse(bad), Err(Error::Invalid)), "{bad}");
    }
    assert!(matches!(
        Endpoint::parse(&format!("wss://{}/connect", "a".repeat(256))),
        Err(Error::Invalid)
    ));
    let custom = Endpoint::parse("wss://gateway.test:8443/connect").unwrap();
    assert_eq!(
        (custom.host.as_str(), custom.port, custom.authority.as_str()),
        ("gateway.test", 8443, "gateway.test:8443")
    );
    let ipv6 = Endpoint::parse("wss://[::1]:444/connect").unwrap();
    assert_eq!((ipv6.host.as_str(), ipv6.port), ("::1", 444));
}

#[test]
fn connect_header_parser_rejects_invalid_fields() {
    for good in ["X-Test: value", "Proxy-Agent:\tserver", "Content-Length: 0"] {
        assert!(valid_connect_header_line(good), "{good}");
    }
    for bad in [
        "NoColon",
        " Bad: value",
        "Bad Name: value",
        "X:\0",
        "X: bad\rvalue",
        "X: \u{7f}",
    ] {
        assert!(!valid_connect_header_line(bad), "{bad:?}");
    }
}

#[tokio::test]
async fn connect_response_bounds_and_authority_preserve_tunnel_bytes() {
    for (endpoint, authority) in [
        ("wss://gateway.test/connect", "gateway.test:443"),
        ("wss://gateway.test:8443/connect", "gateway.test:8443"),
        ("wss://[2001:db8::1]/connect", "[2001:db8::1]:443"),
    ] {
        let (client, mut proxy) = tokio::io::duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let request = read_request(&mut proxy).await;
            proxy
                .write_all(b"HTTP/1.1 200 Connected\r\n\r\nTUNNEL")
                .await
                .unwrap();
            request
        });
        let mut client: BoxedStream = Box::new(client);
        connect_tunnel(&mut client, &Endpoint::parse(endpoint).unwrap(), None)
            .await
            .unwrap();
        let mut suffix = [0; 6];
        client.read_exact(&mut suffix).await.unwrap();
        assert_eq!(&suffix, b"TUNNEL");
        assert_eq!(
            peer.await.unwrap(),
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n")
        );
    }
    for response in [
        format!(
            "HTTP/1.1 200 OK\r\nX: {}\r\n\r\n",
            "x".repeat(MAX_CONNECT_HEADERS)
        ),
        format!(
            "HTTP/1.1 200 OK\r\n{}\r\n",
            "X: value\r\n".repeat(MAX_CONNECT_FIELDS + 1)
        ),
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n".into(),
        "HTTP/1.1 200 OK\r\nX-Invalid : value\r\n\r\n".into(),
    ] {
        let (client, mut proxy) = tokio::io::duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let _ = read_request(&mut proxy).await;
            proxy.write_all(response.as_bytes()).await.unwrap();
        });
        let mut client: BoxedStream = Box::new(client);
        let result = timeout(
            Duration::from_secs(1),
            connect_tunnel(
                &mut client,
                &Endpoint::parse("wss://gateway.test/connect").unwrap(),
                None,
            ),
        )
        .await
        .unwrap();
        assert_eq!(result, Err(Error::Proxy));
        peer.await.unwrap();
    }
}

#[test]
fn resolver_results_and_root_budget_are_bounded() {
    let private = SocketAddr::from(([127, 0, 0, 1], 8443));
    assert_eq!(validate_addresses(vec![private], 8443), Ok(vec![private]));
    assert_eq!(validate_addresses(vec![], 8443), Err(Error::Invalid));
    assert_eq!(
        validate_addresses(vec![private; MAX_ADDRESSES + 1], 8443),
        Err(Error::Invalid)
    );
    assert_eq!(validate_addresses(vec![private], 443), Err(Error::Invalid));
    let (root, _, _) = ca_signed_for("gateway.test");
    assert!(matches!(
        config_from_roots(Vec::new()),
        Err(Error::Unavailable)
    ));
    assert!(matches!(
        config_from_roots(vec![root; MAX_ROOTS + 1]),
        Err(Error::Unavailable)
    ));
}

#[tokio::test]
async fn verified_tls_sends_exact_upgrade_and_retains_socket_slot() {
    let _serial = TEST_SOCKET.lock().await;
    for selected in [Some(protobuf::SUBPROTOCOL), None] {
        let (root, cert, key) = ca_signed_for("gateway.test");
        let (address, peer) = ws_server(cert, key, selected).await;
        let client = client(root, address);
        let connection = client
            .connect(
                &endpoint(address),
                "synthetic-token",
                &CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            connection.protocol(),
            if selected.is_some() {
                Negotiated::ProtobufV2
            } else {
                Negotiated::JsonV1
            }
        );
        assert_eq!(client.0.slot.available_permits(), 0);
        // A recreated owner shares admission with the socket already handed to transport.
        let other = Client::with_config(client.0.tls.clone(), Arc::new(native_resolve));
        assert_eq!(other.0.slot.available_permits(), 0);
        assert!(matches!(
            other
                .connect(
                    &endpoint(address),
                    "synthetic",
                    &CancellationToken::new(),
                    None
                )
                .await,
            Err(Error::Busy)
        ));
        close(connection).await;
        assert_eq!(client.0.slot.available_permits(), 1);
        let request = peer.await.unwrap().to_ascii_lowercase();
        assert!(request.starts_with("get /connect http/1.1\r\n"));
        for field in [
            format!("host: gateway.test:{}", address.port()),
            "authorization: bearer synthetic-token".into(),
            "sec-websocket-protocol: hmux-home.pb.v2.controls1".into(),
        ] {
            assert!(request.contains(&field), "missing {field}");
        }
        assert_eq!(request.matches("authorization:").count(), 1);
    }
}

#[tokio::test]
async fn http_connect_sends_proxy_basic_only_to_proxy_and_retains_slot() {
    let _serial = TEST_SOCKET.lock().await;
    let (home_root, home_cert, home_key) = ca_signed_for("gateway.test");
    let (home_address, home_peer) = ws_server(home_cert, home_key, None).await;
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let proxy_peer = tokio::spawn(async move {
        let (mut incoming, _) = proxy_listener.accept().await.unwrap();
        let request = read_request(&mut incoming).await;
        incoming
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut home = TcpStream::connect(home_address).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut incoming, &mut home).await;
        request
    });
    let client = proxied_client(
        vec![home_root],
        proxy_address,
        &format!("http://user:p%40ss@proxy.test:{}", proxy_address.port()),
    );
    let connection = client
        .connect(
            &endpoint(home_address),
            "home-secret",
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(client.0.slot.available_permits(), 0);
    close(connection).await;
    assert_eq!(client.0.slot.available_permits(), 1);
    let connect = proxy_peer.await.unwrap().to_ascii_lowercase();
    assert!(connect.starts_with(&format!(
        "connect gateway.test:{} http/1.1\r\n",
        home_address.port()
    )));
    assert!(connect.contains(&format!("host: gateway.test:{}\r\n", home_address.port())));
    assert!(connect.contains("proxy-authorization: basic dxnlcjpwqhnz\r\n"));
    assert!(!connect.contains("home-secret"));
    assert!(!connect.contains("authorization: bearer"));
    let home = home_peer.await.unwrap().to_ascii_lowercase();
    assert!(home.contains("authorization: bearer home-secret"));
    assert!(!home.contains("proxy-authorization"));
}

#[tokio::test]
async fn https_connect_verifies_proxy_then_original_home_name() {
    let _serial = TEST_SOCKET.lock().await;
    let (home_root, home_cert, home_key) = ca_signed_for("gateway.test");
    let (proxy_root, proxy_cert, proxy_key) = ca_signed_for("proxy.test");
    let (home_address, home_peer) = ws_server(home_cert, home_key, None).await;
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config(proxy_cert, proxy_key));
    let proxy_peer = tokio::spawn(async move {
        let (socket, _) = proxy_listener.accept().await.unwrap();
        let mut incoming = acceptor.accept(socket).await.unwrap();
        assert_eq!(incoming.get_ref().1.server_name(), Some("proxy.test"));
        let request = read_request(&mut incoming).await;
        incoming
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut home = TcpStream::connect(home_address).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut incoming, &mut home).await;
        request
    });
    let client = proxied_client(
        vec![home_root, proxy_root],
        proxy_address,
        &format!("https://proxy.test:{}", proxy_address.port()),
    );
    let connection = client
        .connect(
            &endpoint(home_address),
            "home-secret",
            &CancellationToken::new(),
            None,
        )
        .await
        .unwrap();
    close(connection).await;
    let connect = proxy_peer.await.unwrap().to_ascii_lowercase();
    assert!(connect.starts_with(&format!(
        "connect gateway.test:{} http/1.1\r\n",
        home_address.port()
    )));
    assert!(!connect.contains("home-secret"));
    let home = home_peer.await.unwrap().to_ascii_lowercase();
    assert!(home.contains("authorization: bearer home-secret"));
}

#[tokio::test]
async fn socks5_and_socks5h_forward_hostname_and_verify_target_tls() {
    let _serial = TEST_SOCKET.lock().await;
    for scheme in ["socks5", "socks5h"] {
        let (root, cert, key) = ca_signed_for("gateway.test");
        let (home_address, home_peer) = ws_server(cert, key, None).await;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 4];
            socket.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 2, 0, 2]);
            socket.write_all(&[5, 2]).await.unwrap();
            let mut credentials = [0u8; 13];
            socket.read_exact(&mut credentials).await.unwrap();
            assert_eq!(&credentials, b"\x01\x04user\x06p@sswd");
            socket.write_all(&[1, 0]).await.unwrap();
            let mut target = [0u8; 5];
            socket.read_exact(&mut target).await.unwrap();
            assert_eq!(target, [5, 1, 0, 3, 12]);
            let mut address_bytes = [0u8; 14];
            socket.read_exact(&mut address_bytes).await.unwrap();
            assert_eq!(&address_bytes[..12], b"gateway.test");
            assert_eq!(&address_bytes[12..], &home_address.port().to_be_bytes());
            socket
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            let mut home = TcpStream::connect(home_address).await.unwrap();
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut home).await;
        });
        let client = proxied_client(
            vec![root],
            address,
            &format!("{scheme}://user:p%40sswd@proxy.test:{}", address.port()),
        );
        let connection = client
            .connect(
                &endpoint(home_address),
                "home-secret",
                &CancellationToken::new(),
                None,
            )
            .await
            .unwrap();
        close(connection).await;
        peer.await.unwrap();
        assert!(home_peer
            .await
            .unwrap()
            .to_ascii_lowercase()
            .contains("authorization: bearer home-secret"));
    }
}

#[tokio::test]
async fn socks_failures_and_malformed_replies_do_not_send_home_bearer() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, _) = cert_for("gateway.test", false);
    for (method, reply) in [
        ([5, 0xff], None),
        ([4, 0], None),
        ([5, 0], Some(vec![5, 5, 0, 1])),
        ([5, 0], Some(vec![5, 0, 1, 1])),
        ([5, 0], Some(vec![5, 0, 0, 9])),
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut greeting = [0u8; 3];
            socket.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            socket.write_all(&method).await.unwrap();
            if let Some(reply) = reply {
                let mut target = [0u8; 5];
                socket.read_exact(&mut target).await.unwrap();
                assert_eq!(target, [5, 1, 0, 3, 12]);
                let mut address_bytes = [0u8; 14];
                socket.read_exact(&mut address_bytes).await.unwrap();
                socket.write_all(&reply).await.unwrap();
            }
            let mut remaining = Vec::new();
            let _ = socket.read_to_end(&mut remaining).await;
            assert!(!remaining.windows(11).any(|w| w == b"home-secret"));
        });
        let client = proxied_client(
            vec![root.clone()],
            address,
            &format!("socks5://proxy.test:{}", address.port()),
        );
        let result = client
            .connect(
                "wss://gateway.test/connect",
                "home-secret",
                &CancellationToken::new(),
                None,
            )
            .await;
        assert!(matches!(result, Err(Error::Proxy)));
        peer.await.unwrap();
        assert_eq!(client.0.slot.available_permits(), 1);
    }
}

#[tokio::test]
async fn socks_auth_rejection_and_wrong_target_certificate_are_terminal() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, _) = cert_for("gateway.test", false);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0u8; 4];
        socket.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 2, 0, 2]);
        socket.write_all(&[5, 2]).await.unwrap();
        let mut credentials = [0u8; 11];
        socket.read_exact(&mut credentials).await.unwrap();
        assert_eq!(&credentials, b"\x01\x04user\x04pass");
        socket.write_all(&[1, 1]).await.unwrap();
        let mut leftover = Vec::new();
        socket.read_to_end(&mut leftover).await.unwrap();
        assert!(leftover.is_empty());
    });
    let client = proxied_client(
        vec![root],
        address,
        &format!("socks5://user:pass@proxy.test:{}", address.port()),
    );
    assert!(matches!(
        client
            .connect(
                "wss://gateway.test/connect",
                "home-secret",
                &CancellationToken::new(),
                None
            )
            .await,
        Err(Error::Proxy)
    ));
    peer.await.unwrap();

    let (wrong_cert, wrong_key) = cert_for("other.test", false);
    let server_cert = wrong_cert.clone();
    let home_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let home_address = home_listener.local_addr().unwrap();
    let home_peer = tokio::spawn(async move {
        let (socket, _) = home_listener.accept().await.unwrap();
        let acceptor = TlsAcceptor::from(server_config(server_cert, wrong_key));
        match acceptor.accept(socket).await {
            Ok(mut tls) => read_request(&mut tls).await,
            Err(_) => String::new(),
        }
    });
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let proxy_peer = tokio::spawn(async move {
        let (mut socket, _) = proxy_listener.accept().await.unwrap();
        let mut greeting = [0u8; 3];
        socket.read_exact(&mut greeting).await.unwrap();
        socket.write_all(&[5, 0]).await.unwrap();
        let mut target = [0u8; 19];
        socket.read_exact(&mut target).await.unwrap();
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        let mut home = TcpStream::connect(home_address).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut socket, &mut home).await;
    });
    let client = proxied_client(
        vec![wrong_cert],
        proxy_address,
        &format!("socks5h://proxy.test:{}", proxy_address.port()),
    );
    assert!(matches!(
        client
            .connect(
                &endpoint(home_address),
                "home-secret",
                &CancellationToken::new(),
                None
            )
            .await,
        Err(Error::Tls)
    ));
    assert!(!home_peer.await.unwrap().contains("home-secret"));
    proxy_peer.await.unwrap();
    assert_eq!(client.0.slot.available_permits(), 1);
}

#[tokio::test]
async fn rejected_connect_cannot_send_home_bearer_or_retry() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, _) = cert_for("gateway.test", false);
    for response in [
        "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
        "HTTP/1.1 302 Found\r\nLocation: http://other.test/\r\n\r\n",
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        });
        let client = proxied_client(
            vec![root.clone()],
            address,
            &format!("http://proxy.test:{}", address.port()),
        );
        let result = client
            .connect(
                "wss://gateway.test/connect",
                "home-secret",
                &CancellationToken::new(),
                None,
            )
            .await;
        assert!(matches!(result, Err(Error::Proxy)));
        let connect = peer.await.unwrap().to_ascii_lowercase();
        assert!(!connect.contains("home-secret"));
        assert_eq!(client.0.slot.available_permits(), 1);
    }
}

#[tokio::test]
async fn cancellation_during_connect_closes_socket_and_releases_slot() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, _) = cert_for("gateway.test", false);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (ready, accepted) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        ready.send(()).unwrap();
        let mut b = [0u8; 1];
        assert_eq!(socket.read(&mut b).await.unwrap(), 0);
        request
    });
    let client = proxied_client(
        vec![root],
        address,
        &format!("http://proxy.test:{}", address.port()),
    );
    let cancel = CancellationToken::new();
    let control = cancel.clone();
    let owner = client.clone();
    let dial = tokio::spawn(async move {
        owner
            .connect("wss://gateway.test/connect", "home-secret", &control, None)
            .await
    });
    timeout(Duration::from_secs(2), accepted)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.0.slot.available_permits(), 0);
    cancel.cancel();
    assert!(matches!(dial.await.unwrap(), Err(Error::Cancelled)));
    assert_eq!(client.0.slot.available_permits(), 1);
    let request = timeout(Duration::from_secs(2), peer)
        .await
        .unwrap()
        .unwrap();
    assert!(!request.contains("home-secret"));
}

#[tokio::test]
async fn wrong_home_name_through_proxy_never_sends_bearer() {
    let _serial = TEST_SOCKET.lock().await;
    let (wrong_cert, wrong_key) = cert_for("other.test", false);
    let home_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let home_address = home_listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config(wrong_cert.clone(), wrong_key));
    let home_peer = tokio::spawn(async move {
        let (socket, _) = home_listener.accept().await.unwrap();
        match acceptor.accept(socket).await {
            Ok(mut tls) => read_request(&mut tls).await,
            Err(_) => String::new(),
        }
    });
    let proxy_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let proxy_address = proxy_listener.local_addr().unwrap();
    let proxy_peer = tokio::spawn(async move {
        let (mut incoming, _) = proxy_listener.accept().await.unwrap();
        let request = read_request(&mut incoming).await;
        incoming
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .unwrap();
        let mut home = TcpStream::connect(home_address).await.unwrap();
        let _ = tokio::io::copy_bidirectional(&mut incoming, &mut home).await;
        request
    });
    let client = proxied_client(
        vec![wrong_cert],
        proxy_address,
        &format!("http://proxy.test:{}", proxy_address.port()),
    );
    let result = client
        .connect(
            &endpoint(home_address),
            "home-secret",
            &CancellationToken::new(),
            None,
        )
        .await;
    assert!(matches!(result, Err(Error::Tls)));
    assert!(!home_peer.await.unwrap().contains("home-secret"));
    assert!(!proxy_peer.await.unwrap().contains("home-secret"));
    assert_eq!(client.0.slot.available_permits(), 1);
}

#[tokio::test]
async fn wrong_root_name_and_expired_certificate_never_send_auth() {
    let _serial = TEST_SOCKET.lock().await;
    for mode in ["root", "name", "expired"] {
        let (cert, key) = cert_for(
            if mode == "name" {
                "other.test"
            } else {
                "gateway.test"
            },
            mode == "expired",
        );
        let trusted = if mode == "root" {
            cert_for("gateway.test", false).0
        } else {
            cert.clone()
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(server_config(cert, key));
        let peer = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            match acceptor.accept(socket).await {
                Ok(mut tls) => read_request(&mut tls).await,
                Err(_) => String::new(),
            }
        });
        let result = client(trusted, address)
            .connect(
                &endpoint(address),
                "synthetic-secret",
                &CancellationToken::new(),
                None,
            )
            .await;
        assert!(matches!(result, Err(Error::Tls)), "{mode}");
        assert!(peer.await.unwrap().is_empty(), "{mode} sent auth bytes");
    }
}

#[tokio::test]
async fn cancelled_dns_worker_holds_global_slot_and_never_later_dials() {
    let _serial = TEST_SOCKET.lock().await;
    let (cert, _) = cert_for("gateway.test", false);
    let tls = config_from_roots(vec![cert]).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (started, wait_started) = oneshot::channel();
    let started = Mutex::new(Some(started));
    let blocked = gate.clone();
    let resolver: Arc<Resolver> = Arc::new(move |_, port| {
        started.lock().unwrap().take().unwrap().send(()).unwrap();
        let (lock, cv) = &*blocked;
        let mut open = lock.lock().unwrap();
        while !*open {
            open = cv.wait(open).unwrap();
        }
        Ok(vec![SocketAddr::new(address.ip(), port)])
    });
    let owner = Client::with_config(tls.clone(), resolver);
    let cancel = CancellationToken::new();
    let control = cancel.clone();
    let url = endpoint(address);
    let running = tokio::spawn({
        let owner = owner.clone();
        async move { owner.connect(&url, "synthetic", &control, None).await }
    });
    timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    assert!(matches!(running.await.unwrap(), Err(Error::Cancelled)));
    let recreated = Client::with_config(tls, Arc::new(native_resolve));
    assert!(matches!(
        recreated
            .connect(
                &endpoint(address),
                "synthetic",
                &CancellationToken::new(),
                None
            )
            .await,
        Err(Error::Busy)
    ));
    assert_eq!(owner.0.slot.available_permits(), 0);
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    timeout(Duration::from_secs(2), async {
        while owner.0.slot.available_permits() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(timeout(Duration::from_millis(100), listener.accept())
        .await
        .is_err());
}

#[tokio::test]
async fn stalled_tls_and_upgrade_cancel_or_timeout_close_socket() {
    let _serial = TEST_SOCKET.lock().await;
    let (cert, key) = cert_for("gateway.test", false);
    for stage in ["tls", "upgrade"] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let tls = config_from_roots(vec![cert.clone()]).unwrap();
        let client = Client::with_config(
            tls,
            Arc::new(move |_, port| Ok(vec![SocketAddr::new(address.ip(), port)])),
        );
        let acceptor = TlsAcceptor::from(server_config(cert.clone(), key.clone_key()));
        let (ready, wait_ready) = oneshot::channel();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if stage == "tls" {
                let _ = ready.send(());
                let mut b = [0u8; 16];
                while socket.read(&mut b).await.unwrap_or(0) != 0 {}
            } else {
                let mut tls = acceptor.accept(socket).await.unwrap();
                let request = read_request(&mut tls).await;
                assert!(request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer synthetic"));
                let _ = ready.send(());
                let mut b = [0u8; 16];
                while tls.read(&mut b).await.unwrap_or(0) != 0 {}
            }
        });
        let cancel = CancellationToken::new();
        let control = cancel.clone();
        let url = endpoint(address);
        let task = tokio::spawn(async move {
            client
                .connect(
                    &url,
                    "synthetic",
                    &control,
                    Some(Instant::now() + Duration::from_millis(300)),
                )
                .await
        });
        timeout(Duration::from_secs(2), wait_ready)
            .await
            .unwrap()
            .unwrap();
        if stage == "tls" {
            cancel.cancel();
        }
        let result = task.await.unwrap();
        assert!(
            matches!(result, Err(Error::Cancelled) | Err(Error::Timeout)),
            "{stage}"
        );
        timeout(Duration::from_secs(2), peer)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn dropped_root_loader_retains_admission_until_actual_exit() {
    let cache: &'static OnceLock<Arc<ClientConfig>> = Box::leak(Box::new(OnceLock::new()));
    let admission: &'static OnceLock<Arc<Semaphore>> = Box::leak(Box::new(OnceLock::new()));
    let (cert, _) = cert_for("gateway.test", false);
    let tls = config_from_roots(vec![cert]).unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (started, wait_started) = oneshot::channel();
    let blocked = gate.clone();
    let expected = tls.clone();
    let first = tokio::spawn(async move {
        load_tls(cache, admission, ROOT_LOAD_TIMEOUT, move || {
            started.send(()).ok();
            let (lock, cv) = &*blocked;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            Ok(expected)
        })
        .await
    });
    timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    first.abort();
    assert!(first.await.is_err());
    assert!(matches!(
        load_tls(cache, admission, ROOT_LOAD_TIMEOUT, || panic!(
            "second loader started"
        ))
        .await,
        Err(Error::Busy)
    ));
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    timeout(Duration::from_secs(2), async {
        while cache.get().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let invoked = Arc::new(AtomicUsize::new(0));
    let count = invoked.clone();
    let cached = load_tls(cache, admission, ROOT_LOAD_TIMEOUT, move || {
        count.fetch_add(1, Ordering::SeqCst);
        panic!("cached loader ran")
    })
    .await
    .unwrap();
    assert!(Arc::ptr_eq(&cached, &tls));
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "requires the host's native TLS trust store; synthetic roots cover normal CI"]
fn native_trust_initialization_is_opt_in() {
    hmux_core::runtime::run_process(async {
        Client::new().await.unwrap();
    })
    .unwrap();
}

#[tokio::test]
async fn abort_during_stalled_tls_drops_socket_and_releases_slot() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, _, _) = ca_signed_for("gateway.test");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let owner = Client::with_config(
        config_from_roots(vec![root]).unwrap(),
        Arc::new(move |_, port| Ok(vec![SocketAddr::new(address.ip(), port)])),
    );
    let (ready, accepted) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        ready.send(()).unwrap();
        let mut bytes = [0u8; 1024];
        while socket.read(&mut bytes).await.unwrap_or(0) != 0 {}
    });
    let url = endpoint(address);
    let task = tokio::spawn({
        let client = owner.clone();
        async move {
            client
                .connect(&url, "synthetic", &CancellationToken::new(), None)
                .await
        }
    });
    timeout(Duration::from_secs(2), accepted)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.0.slot.available_permits(), 0);
    task.abort();
    assert!(task.await.is_err());
    timeout(Duration::from_secs(2), peer)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.0.slot.available_permits(), 1);
}

#[tokio::test]
async fn root_loader_timeout_keeps_slot_until_worker_exits() {
    let cache: &'static OnceLock<Arc<ClientConfig>> = Box::leak(Box::new(OnceLock::new()));
    let admission: &'static OnceLock<Arc<Semaphore>> = Box::leak(Box::new(OnceLock::new()));
    let (root, _, _) = ca_signed_for("gateway.test");
    let tls = config_from_roots(vec![root]).unwrap();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (started, wait_started) = oneshot::channel();
    let blocked = gate.clone();
    let expected = tls.clone();
    let loading = tokio::spawn(async move {
        load_tls(cache, admission, Duration::from_millis(50), move || {
            started.send(()).ok();
            let (lock, cv) = &*blocked;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
            Ok(expected)
        })
        .await
    });
    timeout(Duration::from_secs(2), wait_started)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(loading.await.unwrap(), Err(Error::Timeout)));
    assert!(matches!(
        load_tls(cache, admission, ROOT_LOAD_TIMEOUT, || panic!(
            "second loader started"
        ))
        .await,
        Err(Error::Busy)
    ));
    {
        let (lock, cv) = &*gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    timeout(Duration::from_secs(2), async {
        while cache.get().is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(Arc::ptr_eq(cache.get().unwrap(), &tls));
}

#[tokio::test]
async fn usage_streams_share_two_slots_without_occupying_gateway_admission() {
    let _serial = TEST_SOCKET.lock().await;
    let (root, cert, key) = ca_signed_for("gateway.test");
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = TlsAcceptor::from(server_config(cert, key));
    let peer = tokio::spawn(async move {
        let mut streams = Vec::new();
        for _ in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            streams.push(acceptor.accept(socket).await.unwrap());
        }
        for mut socket in streams {
            let mut data = Vec::new();
            let _ = socket.read_to_end(&mut data).await;
            assert!(data.is_empty());
        }
    });
    let client = client(root, address);
    let gateway = client.0.slot.clone().try_acquire_owned().unwrap();
    let authority = format!("gateway.test:{}", address.port());
    let first = client.usage_tls(&authority).await.unwrap();
    let second = client.clone().usage_tls(&authority).await.unwrap();
    let other = Client::with_config(client.0.tls.clone(), Arc::new(native_resolve));
    assert!(matches!(
        other.usage_tls(&authority).await,
        Err(Error::Busy)
    ));
    assert_eq!(client.0.slot.available_permits(), 0);
    drop(first);
    drop(second);
    drop(gateway);
    assert_eq!(USAGE_SLOTS.get().unwrap().available_permits(), 2);
    assert_eq!(client.0.slot.available_permits(), 1);
    timeout(Duration::from_secs(1), peer)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn socks_literal_targets_match_go_address_types() {
    for (target, encoded) in [
        ("192.0.2.1", vec![1, 192, 0, 2, 1]),
        ("[::ffff:192.0.2.1]", vec![1, 192, 0, 2, 1]),
        (
            "[2001:db8::1]",
            vec![4, 0x20, 1, 0xd, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
        ),
    ] {
        let endpoint = Endpoint::parse(&format!("wss://{target}:443/connect")).unwrap();
        let (left, mut right) = tokio::io::duplex(512);
        let mut stream: BoxedStream = Box::new(left);
        let peer = async move {
            let mut greeting = [0; 3];
            right.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            right.write_all(&[5, 0]).await.unwrap();
            let mut request = vec![0; 3 + encoded.len() + 2];
            right.read_exact(&mut request).await.unwrap();
            assert_eq!(&request[..3], &[5, 1, 0]);
            assert_eq!(&request[3..3 + encoded.len()], encoded.as_slice());
            assert_eq!(&request[3 + encoded.len()..], &443u16.to_be_bytes());
            right
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
        };
        let (result, _) = tokio::join!(connect_socks5(&mut stream, &endpoint, None), peer);
        result.unwrap();
    }
}
