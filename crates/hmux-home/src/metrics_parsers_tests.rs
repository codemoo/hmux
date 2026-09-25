use super::metrics_parsers::{
    cpu_darwin, cpu_linux, disk_bytes, gpu_darwin, memory_darwin, memory_linux,
};

const VM: &str = "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nAnonymous pages: 100.\nPages wired down: 20.\nPages purgeable: 10.\nPages occupied by compressor: 5.\n";

#[test]
fn plist_predefined_and_character_references_need_no_dtd() {
    assert_eq!(gpu_darwin(br#"<plist><dict><key>Name</key><string>A&amp;B&lt;C&gt;&quot;&apos;</string><key>Device Utilization &#37;</key><real>&#x32;5</real></dict></plist>"#),Some(25.0));
    assert_eq!(gpu_darwin(br#"<plist><dict><key>Device Utilization %</key><real>25</real><key>Name</key><string>&unknown;</string></dict></plist>"#),None);
}

#[test]
fn darwin_cpu_uses_interval_sample_and_validates_cpu_only_output() {
    let raw = b"      cpu\n us sy id\n  2  3 95\n 12.5 7.5 80.0\n";
    assert_eq!(cpu_darwin(raw), Some(20.0));
    for raw in [
        "",
        "cpu\nus sy id\n1 1 98\n",
        "cpu\nus sy id\n1 1 98\n1 1 101\n",
        "cpu\nus sy id\n1 1 98\n1 1 NaN\n",
        "cpu\nus sy id\n1 1 98\n-1 1 100\n",
        "cpu\nus sy id\n1 1 98\n1 101 0\n",
        "cpu\nus sy id\nNaN 1 98\n1 1 98\n",
        "cpu\nus sy id\n1 1 98\n1 1 98\nextra\n",
        "cpu\nus id sy\n1 1 98\n1 1 98\n",
        "cpu load average\nus sy id 1m 5m 15m\n1 1 98 2 2 2\n1 1 98 2 2 2\n",
    ] {
        assert_eq!(cpu_darwin(raw.as_bytes()), None, "{raw}");
    }
    assert_eq!(cpu_darwin(&vec![b'x'; 65_537]), None);
}

#[test]
fn darwin_memory_uses_resident_formula_and_bounds() {
    assert_eq!(
        memory_darwin(VM.as_bytes(), b"1048576\n"),
        Some((115 * 4096, 1_048_576))
    );
    let no_anonymous = VM.replace("Anonymous pages", "Other pages");
    assert_eq!(memory_darwin(no_anonymous.as_bytes(), b"1048576"), None);
    assert_eq!(memory_darwin(VM.as_bytes(), b"10"), None);
    let overflow = VM.replace("100.", "18446744073709551615.");
    assert_eq!(memory_darwin(overflow.as_bytes(), b"1048576"), None);
    assert_eq!(memory_darwin(VM.as_bytes(), b"1152921504606846977"), None);
    let saturated = VM.replace("Pages purgeable: 10.", "Pages purgeable: 1000.");
    assert_eq!(
        memory_darwin(saturated.as_bytes(), b"1048576"),
        Some((25 * 4096, 1_048_576))
    );
    assert_eq!(memory_darwin(&vec![b'x'; 65_537], b"1048576"), None);
}

#[test]
fn linux_cpu_excludes_guest_and_rejects_reset_or_overflow() {
    let first = b"cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3 4\n";
    let second = b"cpu  150 0 150 780 120 0 0 0 5 0\n";
    assert_eq!(cpu_linux(first, second), Some(50.0));
    assert_eq!(cpu_linux(second, first), None);
    assert_eq!(cpu_linux(b"x", second), None);
    assert_eq!(cpu_linux(b"cpu 1 2 3 4", b"cpu 1 2 3 4"), None);
    assert_eq!(cpu_linux(b"cpu 18446744073709551615 1 0 1", second), None);
    assert_eq!(cpu_linux(b"cpu 0 0 0 5", b"cpu 1 0 0 4"), None);
    assert_eq!(cpu_linux(&vec![b'x'; 65_537], second), None);
}

#[test]
fn linux_memory_checks_units_overflow_and_total() {
    let raw = b"MemTotal: 16000000 kB\nMemFree: 1000 kB\nMemAvailable: 4000000 kB\n";
    assert_eq!(
        memory_linux(raw),
        Some((12_000_000 * 1024, 16_000_000 * 1024))
    );
    assert_eq!(memory_linux(b"MemTotal: 10 kB\n"), None);
    assert_eq!(
        memory_linux(b"MemTotal: 10 kB\nMemAvailable: 11 kB\n"),
        None
    );
    assert_eq!(
        memory_linux(b"MemTotal: 18446744073709551615 kB\nMemAvailable: 1 kB"),
        None
    );
    assert_eq!(memory_linux(&vec![b'x'; 65_537]), None);
}

#[test]
fn disk_allocation_stays_within_exact_wire_integer_range() {
    assert_eq!(disk_bytes(100, 25, 4096), Some((75 * 4096, 100 * 4096)));
    for (blocks, free, size) in [
        (0, 0, 4096),
        (100, 101, 4096),
        (100, 25, 0),
        (1 << 53, 0, 4096),
    ] {
        assert_eq!(disk_bytes(blocks, free, size), None);
    }
    assert_eq!(disk_bytes(1 << 53, 0, 1), Some((1 << 53, 1 << 53)));
}

#[test]
fn gpu_prefers_canonical_maximum_and_ignores_power() {
    let raw = br#"<?xml version="1.0"?><plist><array><dict>
<key>GPU Power</key><integer>99</integer>
<key>GPU Activity(%)</key><integer>88</integer>
<key>Device Utilization %</key><integer>12</integer>
<key>Device Utilization %</key><real>34.5</real>
</dict></array></plist>"#;
    assert_eq!(gpu_darwin(raw), Some(34.5));
    let apple_plist = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Device Utilization %</key><integer>35</integer></dict></plist>"#;
    assert_eq!(gpu_darwin(apple_plist), Some(35.0));
    assert_eq!(
        gpu_darwin(
            br#"<plist><dict><key>GPU Activity(%)</key><string>42.25</string></dict></plist>"#
        ),
        Some(42.25)
    );
    for raw in [
        r#"<plist><dict><key>GPU Power</key><integer>75</integer></dict></plist>"#,
        r#"<plist><dict><key>Device Utilization %</key><real>NaN</real></dict></plist>"#,
        r#"<plist><dict><key>Device Utilization %</key><integer>101</integer></dict></plist>"#,
        r#"<plist><dict><key>Device Utilization %</key><integer>1</dict></plist>"#,
        r#"<plist><dict><key>Device Utilization %</key><integer>1</integer></dict></plist>trailing"#,
        r#"<!DOCTYPE plist [<!ENTITY x "99">]><plist><dict><key>Device Utilization %</key><integer>&x;</integer></dict></plist>"#,
    ] {
        assert_eq!(gpu_darwin(raw.as_bytes()), None, "{raw}");
    }
}

#[test]
fn gpu_rejects_large_or_deep_xml() {
    assert_eq!(gpu_darwin(&vec![b'x'; 2 * 1024 * 1024 + 1]), None);
    let deep = format!(
        "<plist>{}<key>Device Utilization %</key><integer>25</integer>{}</plist>",
        "<dict>".repeat(32),
        "</dict>".repeat(32)
    );
    assert_eq!(gpu_darwin(deep.as_bytes()), None);
    let large_token = format!("<plist><string>{}</string></plist>", "x".repeat(65_537));
    assert_eq!(gpu_darwin(large_token.as_bytes()), None);
}
