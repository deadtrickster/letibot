use scratch::merge_intervals;

#[test] fn empty() { assert_eq!(merge_intervals(&[]), Vec::<(i64,i64)>::new()); }
#[test] fn single() { assert_eq!(merge_intervals(&[(1,3)]), vec![(1,3)]); }
#[test] fn classic() { assert_eq!(merge_intervals(&[(1,3),(2,6),(8,10),(15,18)]), vec![(1,6),(8,10),(15,18)]); }
#[test] fn touching_merges() { assert_eq!(merge_intervals(&[(1,4),(4,5)]), vec![(1,5)]); }
#[test] fn adjacent_gap_does_not_merge() { assert_eq!(merge_intervals(&[(1,4),(5,6)]), vec![(1,4),(5,6)]); }
#[test] fn unsorted_input() { assert_eq!(merge_intervals(&[(5,6),(1,4),(2,3)]), vec![(1,4),(5,6)]); }
#[test] fn nested() { assert_eq!(merge_intervals(&[(1,10),(2,3)]), vec![(1,10)]); }
#[test] fn negatives() { assert_eq!(merge_intervals(&[(-5,-1),(-2,0)]), vec![(-5,0)]); }
#[test] fn degenerate_points() { assert_eq!(merge_intervals(&[(2,2),(2,2),(5,5)]), vec![(2,2),(5,5)]); }
#[test] fn chain() { assert_eq!(merge_intervals(&[(1,2),(2,3),(3,4),(10,11)]), vec![(1,4),(10,11)]); }
#[test] fn all_one() { assert_eq!(merge_intervals(&[(0,100),(10,20),(30,40),(99,101)]), vec![(0,101)]); }
