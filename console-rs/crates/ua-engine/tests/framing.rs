use ua_engine::{Command, NulFramer, Value, path};

#[test]
fn splits_messages_on_nul() {
    let mut f = NulFramer::new();
    assert_eq!(
        f.push_text(b"{\"path\":\"/a\"}\0{\"pa"),
        vec!["{\"path\":\"/a\"}"]
    );
    assert_eq!(f.push_text(b"th\":\"/b\"}\0"), vec!["{\"path\":\"/b\"}"]);
    assert!(f.push_text(b"\0\0").is_empty());
    assert_eq!(f.pending_bytes(), 0);
}

#[test]
fn encodes_with_trailing_nul() {
    assert_eq!(
        NulFramer::encode("set /Sleep false"),
        b"set /Sleep false\0".to_vec()
    );
}

/// A big frame split across many reads, with small frames before and after it in the same reads.
#[test]
fn reassembles_large_frames_across_chunks() {
    let big = format!("{{\"path\":\"/\",\"data\":\"{}\"}}", "x".repeat(300_000));
    let stream = format!("{{\"path\":\"/a\"}}\0{big}\0{{\"path\":\"/b\"}}\0").into_bytes();
    let mut f = NulFramer::new();
    let mut frames = Vec::new();
    for chunk in stream.chunks(65_536 - 7) {
        frames.extend(f.push_text(chunk));
    }
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0], "{\"path\":\"/a\"}");
    assert_eq!(frames[1], big);
    assert_eq!(frames[2], "{\"path\":\"/b\"}");
    assert_eq!(f.pending_bytes(), 0);
}

#[test]
fn command_text() {
    assert_eq!(Command::get_recursive("/").text(), "get /?recursive=1");
    assert_eq!(
        Command::get_recursive("/x?levels=2").text(),
        "get /x?levels=2&recursive=1"
    );
    assert_eq!(
        Command::get("/SampleRate/value").text(),
        "get /SampleRate/value"
    );
    assert_eq!(
        Command::set_raw("/Sleep", "false").text(),
        "set /Sleep false"
    );
    assert_eq!(Command::SubscribeMulti.text(), "subscribe mode=multi");
    assert_eq!(
        Command::SubscribeBatch(vec!["/a/value".into(), "/b/value".into()]).text(),
        r#"subscribe {"paths":["/a/value","/b/value"]}"#
    );
    assert_eq!(
        Command::Subscribe("/a/value".into()).text(),
        "subscribe /a/value"
    );
    assert_eq!(
        Command::Unsubscribe("/a/value".into()).text(),
        "unsubscribe /a/value"
    );
    assert_eq!(
        Command::identity("networkID", "0123").text(),
        r#"{"path":"networkID","data":"0123"}"#
    );
    assert_eq!(
        Command::identity("client_id", "uaconsole").text(),
        r#"{"path":"client_id","data":"uaconsole"}"#
    );
    assert_eq!(
        Command::post("/SaveLastUsedSession", Some("{}".into())).text(),
        "post /SaveLastUsedSession {}"
    );
    assert_eq!(
        Command::post("/ClearSolos", None).text(),
        "post /ClearSolos"
    );
}

#[test]
fn set_formats_values_for_the_wire() {
    assert_eq!(
        Command::set("/devices/0/inputs/0/FaderLevelTapered", &0.5.into(), false).text(),
        "set /devices/0/inputs/0/FaderLevelTapered/value 0.5"
    );
    assert_eq!(
        Command::set("/UndoRecording/value", &true.into(), false).text(),
        "set /UndoRecording/value true"
    );
    assert_eq!(
        Command::set("/x/EffectName", &"UAD 4K Buss Compressor".into(), false).text(),
        r#"set /x/EffectName/value "UAD 4K Buss Compressor""#
    );
    assert_eq!(
        Command::set("/devices/0/DimAttenuation", &17.0.into(), true).text(),
        "set /devices/0/DimAttenuation/value 17"
    );
    assert_eq!(
        Command::set("/devices/0/DimAttenuation", &17.4.into(), true).text(),
        "set /devices/0/DimAttenuation/value 17"
    );
    assert_eq!(
        Command::set("/x/Name", &r#"a "b""#.into(), false).text(),
        r#"set /x/Name/value "a \"b\"""#
    );
    assert_eq!(
        Command::set("/x/Pan", &Value::Number(-0.5), false).text(),
        "set /x/Pan/value -0.5"
    );
}

#[test]
fn only_set_and_post_are_writes() {
    assert!(Command::set_raw("/a/value", "1").is_write());
    assert!(Command::post("/ClearSolos", None).is_write());
    assert!(!Command::get("/").is_write());
    assert!(!Command::Subscribe("/a/value".into()).is_write());
    assert!(!Command::SubscribeBatch(vec!["/a/value".into()]).is_write());
    assert!(!Command::SubscribeMulti.is_write());
    assert!(!Command::Unsubscribe("/a/value".into()).is_write());
    assert!(!Command::identity("client_id", "uaconsole").is_write());
}

#[test]
fn path_helpers() {
    assert_eq!(path::property_path("/a/Mute/value"), "/a/Mute");
    assert_eq!(path::property_path("/a/Mute/value?func_id=3"), "/a/Mute");
    assert_eq!(path::property_path("/a/Mute"), "/a/Mute");
    assert_eq!(path::value_path("/a/Mute"), "/a/Mute/value");
    assert_eq!(path::value_path("/a/Mute/value"), "/a/Mute/value");
    assert_eq!(path::join("/", "TalkbackOn"), "/TalkbackOn");
    assert_eq!(path::join("/devices/0", "Name"), "/devices/0/Name");
    assert_eq!(path::split("/TalkbackOn"), ("/", "TalkbackOn"));
    assert_eq!(path::split("/devices/0/Name"), ("/devices/0", "Name"));
}
