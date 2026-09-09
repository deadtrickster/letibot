use scratch::parse_range_list;

#[test] fn empty_string() { assert_eq!(parse_range_list(""), Some(vec![])); }
#[test] fn blank_string() { assert_eq!(parse_range_list("   "), Some(vec![])); }
#[test] fn single_number() { assert_eq!(parse_range_list("5"), Some(vec![5])); }
#[test] fn mixed() { assert_eq!(parse_range_list("1-3,5,7-8"), Some(vec![1,2,3,5,7,8])); }
#[test] fn whitespace_tolerant() { assert_eq!(parse_range_list(" 1 - 3 , 5 "), Some(vec![1,2,3,5])); }
#[test] fn degenerate_range() { assert_eq!(parse_range_list("3-3"), Some(vec![3])); }
#[test] fn order_preserved_not_sorted() { assert_eq!(parse_range_list("2,1"), Some(vec![2,1])); }
#[test] fn duplicates_preserved() { assert_eq!(parse_range_list("1-2,1-2"), Some(vec![1,2,1,2])); }
#[test] fn u32_max_ok() { assert_eq!(parse_range_list("4294967295"), Some(vec![4294967295])); }
#[test] fn zero_ok() { assert_eq!(parse_range_list("0-2"), Some(vec![0,1,2])); }
#[test] fn reversed_range_is_none() { assert_eq!(parse_range_list("3-1"), None); }
#[test] fn empty_item_is_none() { assert_eq!(parse_range_list("1,,2"), None); }
#[test] fn trailing_comma_is_none() { assert_eq!(parse_range_list("1,2,"), None); }
#[test] fn letters_are_none() { assert_eq!(parse_range_list("a"), None); }
#[test] fn open_range_is_none() { assert_eq!(parse_range_list("1-"), None); }
#[test] fn leading_dash_is_none() { assert_eq!(parse_range_list("-1"), None); }
#[test] fn overflow_is_none() { assert_eq!(parse_range_list("4294967296"), None); }
#[test] fn triple_dash_is_none() { assert_eq!(parse_range_list("1-2-3"), None); }
