use super::*;

#[test]
fn selectors_hashes_and_environment_preserve_values_and_reject_ambiguous_input() {
    let id = format!("dev_{}", "AB".repeat(16));
    assert_eq!(device_selector(&id).unwrap(), id.to_ascii_lowercase());
    assert_eq!(device_selector("mac1").unwrap(), "mac1");
    for invalid in [
        "",
        "../mac1",
        "Mac 1",
        "dev_123",
        "dev_gggggggggggggggggggggggggggggggg",
    ] {
        assert!(device_selector(invalid).is_err(), "{invalid}");
    }
    let hash = "AB".repeat(32);
    assert_eq!(expect_hash(&hash).unwrap(), hash.to_ascii_lowercase());
    for invalid in ["abc", &"x".repeat(64), &"0".repeat(65)] {
        assert!(expect_hash(invalid).is_err());
    }
    assert_eq!(
        parse_env("TOKEN=a=b").unwrap(),
        ("TOKEN".into(), "a=b".into())
    );
    assert_eq!(
        parse_env("EMPTY=").unwrap(),
        ("EMPTY".into(), String::new())
    );
    for invalid in ["NO_EQUALS", "=value", "NA\0ME=value", "NAME=val\0ue"] {
        assert!(parse_env(invalid).is_err());
    }
}
