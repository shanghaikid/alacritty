use super::*;

fn feed(store: &mut Store, text: &str) -> Outcome {
    store.apply(text.as_bytes(), Terminator::St)
}
fn snapshot(store: &Store) -> Vec<String> {
    store.records().map(|r| format!("{r:?}")).collect()
}

#[test]
fn states_queries_and_no_query_side_effects() {
    let mut store = Store::default();
    assert_eq!(store.apply(b"?", Terminator::St), Outcome::Query(Terminator::St));
    assert_eq!(store.apply(b" ? ", Terminator::Bel), Outcome::Query(Terminator::Bel));
    assert_eq!(store.generation(), 0);
    for state in ["idle", "working", "done", "blocked", "error"] {
        assert_eq!(feed(&mut store, &format!("state={state}")), Outcome::Changed);
        assert_eq!(store.summary().unwrap().state.name(), state);
    }
    let before = snapshot(&store);
    assert_eq!(feed(&mut store, "?:states=working:kinds=auth"), Outcome::Rejected);
    assert_eq!(snapshot(&store), before);
    assert!(!REPLY_ST.contains("msg="));
    assert!(REPLY_ST.ends_with("\x1b\\"));
    assert!(REPLY_BEL.ends_with('\x07'));
}

#[test]
fn replacement_duplicates_unknown_and_malformed_pairs() {
    let mut store = Store::default();
    feed(
        &mut store,
        "state=idle:state=blocked:kind=auth:progress=50:app=tool:title=VGl0bGU=:msg=SGVsbG8=",
    );
    let r = store.summary().unwrap();
    assert_eq!(r.kind, Some(Kind::Auth));
    assert_eq!(r.progress, Some(50));
    assert_eq!(r.title, Some("Title"));
    assert_eq!(r.msg, Some("Hello"));
    feed(&mut store, "state=working:badpair:Bad=x:unknown=ignored:progress=abc:app=a,b");
    let r = store.summary().unwrap();
    assert_eq!(r.state, State::Working);
    assert_eq!((r.kind, r.progress, r.app, r.title, r.msg), (None, None, None, None, None));
    feed(&mut store, " state = done : kind = auth : progress = 23 ");
    let r = store.summary().unwrap();
    assert_eq!((r.state, r.kind, r.progress), (State::Done, None, None));
    assert_eq!(feed(&mut store, "state=idle:state=future"), Outcome::Rejected);
    assert_eq!(feed(&mut store, "state=future:state=idle"), Outcome::Changed);
    feed(&mut store, "state=working:bad=a;b:progress=100");
    assert_eq!(store.summary().unwrap().progress, Some(100));
}

#[test]
fn hierarchical_inheritance_and_segment_aware_clear() {
    let mut store = Store::default();
    for p in [
        "state=idle:app=root",
        "state=working:id=a:app=parent",
        "state=blocked:id=a/b/c:kind=question",
        "state=done:id=abc",
    ] {
        feed(&mut store, p);
    }
    assert_eq!(store.inherited_app("a/b/c"), Some("parent"));
    assert_eq!(store.inherited_app("absent/x"), Some("root"));
    assert_eq!(store.summary().unwrap().id, "a/b/c");
    assert_eq!(store.summary().unwrap().app, Some("parent"));
    feed(&mut store, "state=clear:id=a");
    assert_eq!(store.len(), 2);
    assert!(store.records().any(|r| r.id == "abc"));
    feed(&mut store, "state=clear");
    assert!(store.is_empty());
}

#[test]
fn transient_expiry_reset_and_duplicate_cache_invalidation() {
    let mut store = Store::default();
    for state in ["idle", "working", "blocked", "done", "error"] {
        feed(&mut store, &format!("state={state}:id={state}"));
    }
    assert!(store.expire_transient());
    assert_eq!(store.len(), 2);
    assert!(store.records().all(|r| matches!(r.state, State::Done | State::Error)));
    assert!(!store.expire_transient());
    assert_eq!(feed(&mut store, "state=error:id=error"), Outcome::Changed);
    assert!(store.reset());
    assert_eq!(feed(&mut store, "state=error:id=error"), Outcome::Changed);
    assert_eq!(store.len(), 1);
}

#[test]
fn invalid_ids_never_overwrite_root() {
    let mut store = Store::default();
    feed(&mut store, "state=done:app=root");
    let before = snapshot(&store);
    for id in ["", "/x", "a/", "a//b", "a,b", "a/b/c/d/e/f/g/h/i"] {
        assert_eq!(feed(&mut store, &format!("state=clear:id={id}")), Outcome::Rejected, "{id}");
        assert_eq!(snapshot(&store), before);
    }
    assert_eq!(feed(&mut store, "state=clear:id=a,b:id=ok"), Outcome::Unchanged);
}

#[test]
fn record_limit_and_last_updated_eviction() {
    let mut store = Store::default();
    for i in 0..MAX_RECORDS {
        feed(&mut store, &format!("state=working:id=p{i}"));
    }
    assert_eq!(store.len(), MAX_RECORDS);
    feed(&mut store, "state=working:id=p0");
    feed(&mut store, "state=working:id=new");
    assert_eq!(store.len(), MAX_RECORDS);
    assert!(store.records().any(|r| r.id == "p0"));
    assert!(!store.records().any(|r| r.id == "p1"));
    assert!(store.records().any(|r| r.id == "new"));
}

#[test]
fn duplicate_reports_refresh_lru_without_dirtying_ui() {
    let mut store = Store::default();
    feed(&mut store, "state=working:app=build:progress=40");
    let generation = store.generation();
    for _ in 0..1000 {
        assert_eq!(feed(&mut store, "state=working:app=build:progress=40"), Outcome::Unchanged);
    }
    assert_eq!(store.generation(), generation);
    assert_eq!(feed(&mut store, "state=working:app=build:progress=41"), Outcome::Changed);
}

#[test]
fn invalid_text_and_caps_are_atomic_even_with_duplicates() {
    let mut store = Store::default();
    feed(&mut store, "state=done:app=existing");
    let before = snapshot(&store);
    let generation = store.generation();
    for text in ["Zg=", "Zg===", "____", "AA==", "Cg==", "fw==", "woA=", "/w==", "Zh=="] {
        assert_eq!(
            feed(&mut store, &format!("state=working:msg={text}")),
            Outcome::Rejected,
            "{text}"
        );
        assert_eq!(snapshot(&store), before);
    }
    assert_eq!(feed(&mut store, "state=working:msg=AA==:msg=b2s="), Outcome::Rejected);
    for p in [
        format!("state=working:msg={}", "a".repeat(2733)),
        format!("state=working:title={}", "a".repeat(257)),
        format!("state=working:app={}", "a".repeat(33)),
        format!("state=working:id={}", "a".repeat(33)),
        format!("state=working:{}=x", "a".repeat(17)),
        format!("state=working:unknown={}", "a".repeat(4096)),
        format!("state=working:msg={}:msg=b2s=", "a".repeat(2733)),
    ] {
        assert_eq!(feed(&mut store, &p), Outcome::Rejected);
        assert_eq!(snapshot(&store), before);
    }
    assert_eq!(store.generation(), generation);
}

#[test]
fn maximum_text_lengths_padded_and_unpadded() {
    let mut store = Store::default();
    for padding in [true, false] {
        let title = "t".repeat(MAX_TITLE);
        let message = "m".repeat(MAX_MESSAGE);
        let engine = GeneralPurpose::new(
            &STANDARD,
            GeneralPurposeConfig::new().with_encode_padding(padding),
        );
        let report = format!(
            "state=working:title={}:msg={}",
            engine.encode(&title),
            engine.encode(&message)
        );
        assert_ne!(feed(&mut store, &report), Outcome::Rejected);
        assert_eq!(store.summary().unwrap().msg, Some(message.as_str()));
        assert_eq!(store.summary().unwrap().title, Some(title.as_str()));
    }
    let too_large = BASE64.encode("m".repeat(MAX_MESSAGE + 1));
    assert_eq!(feed(&mut store, &format!("state=working:msg={too_large}")), Outcome::Rejected);
}

#[test]
fn exact_wire_limit_respects_terminator_and_duplicate_fast_path() {
    let mut store = Store::default();
    let mut body = b"state=working:x=".to_vec();
    body.resize(MAX_SEQUENCE - Terminator::Bel.overhead(), b'a');
    assert_eq!(store.apply(&body, Terminator::Bel), Outcome::Changed);
    assert_eq!(store.apply(&body, Terminator::St), Outcome::Rejected);
    assert_eq!(store.apply(&body, Terminator::Bel), Outcome::Unchanged);
    body.pop();
    assert_ne!(store.apply(&body, Terminator::St), Outcome::Rejected);
}

#[test]
fn display_is_bounded_plain_text_and_disarms_invisibles() {
    let mut store = Store::default();
    let message = BASE64.encode("abc\u{202e}def\u{200b}ghi".repeat(20));
    feed(&mut store, &format!("state=blocked:kind=permission:msg={message}"));
    let mut display = String::new();
    store.display_label(&mut display);
    assert!(display.starts_with("[permission] abcdefghi"));
    assert!(display.chars().count() <= 110);
    assert!(!display.chars().any(invisible));
    assert!(store.summary().unwrap().msg.unwrap().contains('\u{202e}'));
}

#[test]
fn progress_validation_recency_and_counter_wrap() {
    let mut store = Store::default();
    for progress in ["-1", "101", "256", "+1", "1.2", ""] {
        feed(&mut store, &format!("state=working:progress={progress}"));
        assert_eq!(store.summary().unwrap().progress, None);
    }
    store.clock = u64::MAX - 1;
    feed(&mut store, "state=done:id=a");
    feed(&mut store, "state=done:id=b");
    feed(&mut store, "state=done:id=c");
    assert_eq!(store.summary().unwrap().id, "c");
}
