use scratch::parse_size;

#[test] fn bare_number() { assert_eq!(parse_size("512"), Some(512)); }
#[test] fn zero() { assert_eq!(parse_size("0"), Some(0)); }
#[test] fn bytes_suffix() { assert_eq!(parse_size("512B"), Some(512)); }
#[test] fn kib() { assert_eq!(parse_size("4KiB"), Some(4096)); }
#[test] fn kib_space() { assert_eq!(parse_size("4 KiB"), Some(4096)); }
#[test] fn kib_padded_lowercase() { assert_eq!(parse_size("  4kib  "), Some(4096)); }
#[test] fn mb_decimal() { assert_eq!(parse_size("1MB"), Some(1_000_000)); }
#[test] fn mib_binary() { assert_eq!(parse_size("1MiB"), Some(1_048_576)); }
#[test] fn gib() { assert_eq!(parse_size("2GiB"), Some(2_147_483_648)); }
#[test] fn tb() { assert_eq!(parse_size("3TB"), Some(3_000_000_000_000)); }
#[test] fn tib() { assert_eq!(parse_size("1TiB"), Some(1_099_511_627_776)); }
#[test] fn mixed_case_kb() { assert_eq!(parse_size("4Kb"), Some(4000)); }
#[test] fn u64_max() { assert_eq!(parse_size("18446744073709551615"), Some(u64::MAX)); }
#[test] fn empty_is_none() { assert_eq!(parse_size(""), None); }
#[test] fn blank_is_none() { assert_eq!(parse_size("   "), None); }
#[test] fn suffix_only_is_none() { assert_eq!(parse_size("KiB"), None); }
#[test] fn trailing_junk_is_none() { assert_eq!(parse_size("4KB junk"), None); }
#[test] fn fractional_is_none() { assert_eq!(parse_size("4.5KiB"), None); }
#[test] fn negative_is_none() { assert_eq!(parse_size("-1"), None); }
#[test] fn unknown_suffix_is_none() { assert_eq!(parse_size("4XiB"), None); }
#[test] fn overflow_is_none() { assert_eq!(parse_size("18446744073709551615KiB"), None); }
#[test] fn big_overflow_number_is_none() { assert_eq!(parse_size("99999999999999999999999"), None); }
