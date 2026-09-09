use scratch::longest_common_prefix;

#[test] fn both_empty() { assert_eq!(longest_common_prefix(&[], &[]), 0); }
#[test] fn one_empty() { assert_eq!(longest_common_prefix(&[1,2,3], &[]), 0); }
#[test] fn other_empty() { assert_eq!(longest_common_prefix(&[], &[1,2,3]), 0); }
#[test] fn identical() { assert_eq!(longest_common_prefix(&[1,2,3], &[1,2,3]), 3); }
#[test] fn proper_prefix() { assert_eq!(longest_common_prefix(&[1,2,3], &[1,2,3,4,5]), 3); }
#[test] fn diverge_at_zero() { assert_eq!(longest_common_prefix(&[9,2,3], &[1,2,3]), 0); }
#[test] fn diverge_midway() { assert_eq!(longest_common_prefix(&[1,2,7,4], &[1,2,3,4]), 2); }
#[test] fn single_match() { assert_eq!(longest_common_prefix(&[7], &[7]), 1); }
#[test] fn big_values() { assert_eq!(longest_common_prefix(&[u32::MAX, 0], &[u32::MAX, 1]), 1); }
#[test] fn long_run() {
    let a: Vec<u32> = (0..1000).collect();
    let mut b: Vec<u32> = (0..1000).collect();
    b[777] = 12345;
    assert_eq!(longest_common_prefix(&a, &b), 777);
}
