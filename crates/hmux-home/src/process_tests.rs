use super::*;

fn snapshot(rows: &str) -> Snapshot {
    Snapshot::parse(rows.as_bytes()).expect("synthetic process table")
}

#[test]
fn provider_and_generic_selection_match_go() {
    let table = snapshot("100 1 Ss 0.0 -zsh\n101 100 S+ 0.0 node\n102 101 S+ 1.2 /opt/tool/@openai/codex/vendor/bin/codex\n103 102 S 0.0 codex-code-mode-host\n200 1 Ss 0.0 -zsh\n201 200 S+ 0.0 claude\n202 201 S 4.0 node\n300 1 Ss 0.0 -zsh\n301 300 S+ 0.0 uv\n302 301 S+ 3.4 /opt/python/bin/Python");
    assert_eq!(table.candidate(100).unwrap().pid, 102);
    assert_eq!(table.candidate(200).unwrap().pid, 201);
    assert_eq!(table.candidate(300).unwrap().process, "Python");
    assert_eq!(table.nearest_provider(100), (102, Status::Ready));
    assert_eq!(table.nearest_provider(200), (201, Status::Ready));
    assert_eq!(table.nearest_provider(300), (0, Status::Unavailable));
    assert_eq!(infer_agent_state(table.nodes.get(&102).unwrap()), "working");
    assert_eq!(infer_agent_state(table.nodes.get(&201).unwrap()), "idle");
}

#[test]
fn foreground_branch_wins_and_nested_provider_is_excluded() {
    let table = snapshot(
        "1 0 Ss 0.0 zsh\n2 1 S 0.0 codex\n3 1 S 0.0 node\n4 3 S+ 0.0 claude\n5 2 R+ 0.0 codex",
    );
    assert_eq!(table.nearest_provider(1), (4, Status::Ready));
    assert_eq!(table.nearest_provider(2), (2, Status::Ready));
    // Candidate follows Go's shallowest-provider display heuristic; it must
    // never be used for an exact session association.
    assert_eq!(table.candidate(1).unwrap().pid, 2);
}

#[test]
fn multiple_provider_branches_are_ambiguous_without_unique_foreground() {
    let table = snapshot("1 0 Ss 0 zsh\n2 1 S 0 codex\n3 1 S 0 claude");
    assert_eq!(table.nearest_provider(1), (0, Status::Ambiguous));
    let table = snapshot("1 0 Ss 0 zsh\n2 1 S+ 0 codex\n3 1 S+ 0 claude");
    assert_eq!(table.nearest_provider(1), (0, Status::Ambiguous));
}

#[test]
fn wrapper_chain_requires_single_non_provider_branch_and_caps_depth() {
    let table = snapshot("20 1 S 0 codex\n21 20 S 0 node\n22 21 S 0 sh");
    assert_eq!(table.wrapper_chain(20), (vec![21, 22], Status::Ready));
    assert_eq!(table.wrapper_chain(21), (vec![], Status::Unavailable));
    let table = snapshot("20 1 S 0 codex\n21 20 S 0 node\n22 20 S 0 sh");
    assert_eq!(table.wrapper_chain(20), (vec![], Status::Ambiguous));
    let table = snapshot("20 1 S 0 codex\n21 20 S 0 claude");
    assert_eq!(table.wrapper_chain(20), (vec![], Status::Ambiguous));
    let mut rows = String::from("20 1 S 0 codex\n");
    for pid in 21..=36 {
        rows.push_str(&format!("{pid} {} S 0 node\n", pid - 1));
    }
    assert_eq!(snapshot(&rows).wrapper_chain(20).1, Status::Ready);
    rows.push_str("37 36 S 0 node\n");
    assert_eq!(
        snapshot(&rows).wrapper_chain(20),
        (vec![], Status::Unavailable)
    );
}

#[test]
fn parse_rejects_oversize_rows_duplicate_and_cycles() {
    assert_eq!(
        Snapshot::parse(&vec![b'x'; MAX_RAW_BYTES + 1]).unwrap_err(),
        Error::RawTooLarge
    );
    assert_eq!(
        Snapshot::parse("1 0 S 0 sh\n1 0 S 0 sh".as_bytes()).unwrap_err(),
        Error::DuplicatePid
    );
    assert_eq!(
        Snapshot::parse("1 2 S 0 sh\n2 1 S 0 sh".as_bytes()).unwrap_err(),
        Error::CyclicParent
    );
    assert_eq!(
        Snapshot::parse("junk".as_bytes()).unwrap_err(),
        Error::NoValidRows
    );
    assert!(Snapshot::parse("\n".as_bytes()).unwrap().nodes.is_empty());
    assert_eq!(
        Snapshot::parse(&vec![b'\n'; MAX_ROWS + 1]).unwrap_err(),
        Error::TooManyRows
    );
    let mut rows = String::new();
    for pid in 1..=MAX_NODES + 1 {
        rows.push_str(&format!("{pid} 0 S 0 sh\n"));
    }
    assert_eq!(
        Snapshot::parse(rows.as_bytes()).unwrap_err(),
        Error::TooManyNodes
    );
}

#[test]
fn traversal_exhaustion_fails_closed_before_queue_growth() {
    let mut rows = String::from("1 0 Ss 0 zsh\n2 1 S+ 0 codex\n");
    for pid in 3..=MAX_TREE_NODES as i32 + 2 {
        rows.push_str(&format!("{pid} 1 S 0 worker\n"));
    }
    let table = snapshot(&rows);
    assert_eq!(table.nearest_provider(1), (0, Status::Ambiguous));
    assert!(table.candidate(1).is_none());
}

#[test]
fn parser_recognizes_versioned_claude_and_rejects_invalid_numbers() {
    let table = snapshot("1 0 S+ 0 /synthetic/.local/share/claude/versions/2.1.263\n2 0 S 0 /synthetic/other/2.1.263\n3 0 S NaN codex\n4 0 S 0 /synthetic/claude/versions/invalid!\n5 0 S 0 codex");
    assert_eq!(table.nodes[&1].provider, Some(Provider::Claude));
    assert_eq!(table.nodes[&2].provider, None);
    assert!(!table.nodes.contains_key(&3));
    assert_eq!(table.nodes[&4].provider, None);
    assert_eq!(table.nodes[&5].provider, Some(Provider::Codex));
}
